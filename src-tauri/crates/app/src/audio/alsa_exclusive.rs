//! Linux-only ALSA hardware-exclusive output backend (#495 for DoP, then
//! ordinary PCM).
//!
//! The Linux equivalent of [`super::wasapi_exclusive`]: it opens the DAC
//! as a **raw `hw:` device** (never `default` / `plughw:` / a Pulse or
//! PipeWire alias), which bypasses the system mixer + resampler and gives
//! us exclusive access to the hardware.
//!
//! It carries two kinds of stream, and the difference between them is
//! the rate:
//!
//! - **DoP** (`Some(DopFormat)`) — opened at the exact DoP rate
//!   (`dsd_rate / 16`) in **`S32_LE`**, each 24-bit DoP word placed
//!   MSB-justified in the 32-bit sample (marker in the top byte) via
//!   [`super::dop_pack::fill_dop_period_i32`]. The rate is a *demand*:
//!   the marker cadence only survives if nothing resamples it, so a
//!   device that can't do this exact rate fails the open and the engine
//!   falls back to ordinary DSD → PCM through cpal shared.
//! - **PCM** (`None`) — the audiophile path for every other track. Here
//!   the rate is a *preference*: the decoder's rubato stage converts to
//!   whatever the device lands on, exactly as it does for cpal shared,
//!   so we take the device's answer and publish it. What exclusive buys
//!   is the mixer's absence, not the source rate — see the note on
//!   source-rate negotiation below.
//!
//! **Scope, same as WASAPI's:** this is "bypass the system mixer",
//! not yet "honor the source rate exactly". `hw:` guarantees no
//! resampling *below* us; the resampling that remains is our own, and
//! moving it out means re-opening the device per track. That's a
//! separate phase, and it is the one that would make the word
//! bit-perfect true end to end.
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

use alsa::pcm::{Access, Format, Frames, HwParams, State, IO, PCM};
use alsa::{Direction, ValueOr};
use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use tauri::AppHandle;

use super::output::{DopFormat, OutputHandle, RING_CAPACITY};
use super::state::SharedPlayback;
use crate::error::{AppError, AppResult};

/// Spawn the ALSA exclusive output thread. Mirrors
/// [`super::wasapi_exclusive::spawn_exclusive_output_thread`]'s contract:
/// returns the decoder-side `Producer<f32>` and an [`OutputHandle`], or an
/// error (device busy / format unsupported / no such device) surfaced
/// synchronously so the caller can fall back — to DSD → PCM for a DoP
/// request, to cpal shared mode for a PCM one.
pub fn spawn_alsa_exclusive_output_thread(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    device_name: Option<String>,
    dop: Option<DopFormat>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<()>>(1);

    let thread_shared = shared.clone();
    let thread_app = app.clone();
    let thread_device = device_name.clone();
    let join: JoinHandle<()> = std::thread::Builder::new()
        .name(match dop {
            Some(_) => "waveflow-alsa-dop".into(),
            None => "waveflow-alsa-exclusive".to_string(),
        })
        .spawn(move || match dop {
            Some(dop) => output_thread_main(
                thread_shared,
                consumer,
                shutdown_rx,
                init_tx,
                thread_app,
                thread_device,
                dop,
            ),
            None => pcm_output_thread_main(
                thread_shared,
                consumer,
                shutdown_rx,
                init_tx,
                thread_app,
                thread_device,
            ),
        })
        .map_err(|e| AppError::Audio(format!("spawn alsa exclusive thread: {e}")))?;

    match init_rx.recv() {
        Ok(Ok(())) => Ok((
            producer,
            OutputHandle {
                shutdown_tx,
                join,
                device_name,
                // We hold the card through a raw `hw:` handle: nothing
                // else can mix into it while this thread lives. That is
                // true of the DoP stream too — it used to report `false`
                // here, which made the pipeline panel deny an exclusive
                // grab that had in fact happened (WASAPI has always
                // reported `true` for both).
                exclusive: true,
                dop,
            },
        )),
        Ok(Err(err)) => {
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(AppError::Audio(
            "alsa exclusive thread died before reporting init result".into(),
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
    let (_reservation, (pcm, period_frames)) =
        match open_reserving_the_card(&dev, "dop", || open_dop_pcm(&dev, dop)) {
            Ok(opened) => opened,
            Err(err) => {
                tracing::warn!(
                    %err,
                    device = %dev,
                    "alsa dop init failed; falling back to DSD -> PCM"
                );
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

        // One `i32` per channel per frame, so the item stride of a frame
        // is the channel count. Re-offering a partial write matters most
        // here: a hole shifts every following frame against the DoP
        // marker cadence the DAC is locked onto.
        match write_full_period(&pcm, &io, &buf, channels, period_frames, &shutdown_rx) {
            PeriodOutcome::Written => {}
            PeriodOutcome::Shutdown => break 'run ExitReason::Shutdown,
            PeriodOutcome::DeviceLost(reason) => break 'run ExitReason::DeviceLost(reason),
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

/// Take the card, asking the sound server to step aside if it holds it.
///
/// Shared by both streams because the obstacle is the same one: on any
/// desktop the holder of a `hw:` device is PipeWire or PulseAudio, which
/// grabbed the card at login. Before the reservation protocol existed the
/// answer to that was always to give up.
///
/// The returned reservation must be kept alive alongside the PCM and
/// dropped with it — holding a card we no longer play on would keep the
/// sound server locked out of it. `None` means the card was free and
/// nothing had to be asked.
///
/// `what` only labels the log lines; the caller still reports the
/// failure in its own terms, because what happens next differs (a DoP
/// stream falls back to DSD → PCM, a PCM stream to cpal shared).
fn open_reserving_the_card<T>(
    dev: &str,
    what: &str,
    mut attempt: impl FnMut() -> Result<T, PcmOpenError>,
) -> Result<(Option<super::device_reservation::Reservation>, T), AppError> {
    match attempt() {
        Ok(opened) => Ok((None, opened)),
        Err(failure) if failure.busy => {
            let Some(reservation) =
                hw_card_index(dev).and_then(super::device_reservation::Reservation::acquire)
            else {
                tracing::warn!(
                    device = %dev,
                    %what,
                    "alsa: the card is busy and could not be reserved"
                );
                return Err(failure.err);
            };
            match open_after_release(attempt) {
                Ok(opened) => Ok((Some(reservation), opened)),
                Err(failure) => {
                    tracing::warn!(
                        err = %failure.err,
                        device = %dev,
                        %what,
                        "alsa: the card stayed busy after the reservation"
                    );
                    Err(failure.err)
                }
            }
        }
        Err(failure) => Err(failure.err),
    }
}

/// Retry the open while the sound server finishes letting go.
///
/// Releasing is asynchronous on its side — it sees `NameLost`, plays
/// out what it has buffered and only then closes the device — so the
/// first open after the reservation lands still returns `EBUSY`.
fn open_after_release<T>(
    mut attempt: impl FnMut() -> Result<T, PcmOpenError>,
) -> Result<T, PcmOpenError> {
    const STEP: Duration = Duration::from_millis(50);
    let deadline = Instant::now() + super::device_reservation::RELEASE_GRACE;
    loop {
        // Try before waiting: a server that let go promptly costs
        // nothing, and a device that simply cannot do what we're asking
        // says so on the first attempt.
        match attempt() {
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
fn open_dop_pcm(dev: &str, dop: DopFormat) -> Result<(PCM, usize), PcmOpenError> {
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
        set_period_and_buffer(&hwp)?;
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

/// Rate asked for when the engine has not settled on one yet (nothing
/// has played since launch). 48 kHz rather than 44.1 because it is the
/// rate a modern DAC is most likely to run natively.
const DEFAULT_PCM_RATE: u32 = 48_000;

/// The wire formats the PCM path will accept from a `hw:` device, best
/// first. Falling all the way through means the device speaks nothing we
/// can write, and the caller drops to cpal shared mode.
///
/// This mirrors [`super::wasapi_exclusive`]'s chain in intent but **not**
/// in layout — see [`AlsaSampleFormat::S24In32`] for the one place where
/// copying the other backend's packer would be a 48 dB mistake.
const FORMAT_FALLBACK_CHAIN: [AlsaSampleFormat; 5] = [
    AlsaSampleFormat::F32,
    AlsaSampleFormat::S32,
    AlsaSampleFormat::S24Packed,
    AlsaSampleFormat::S24In32,
    AlsaSampleFormat::S16,
];

/// A sample format a raw `hw:` device can be opened with.
///
/// There is no plug layer underneath us — that is the entire point of
/// `hw:` — so every one of these is a format the hardware itself
/// accepts, and the conversion from the ring's `f32` is ours to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlsaSampleFormat {
    /// `SND_PCM_FORMAT_FLOAT_LE`. Exactly what the ring already holds,
    /// so the "conversion" is a byte copy. Uncommon on USB DACs (the
    /// converter is integer), routine on HDMI and on some onboard
    /// codecs — worth one attempt for the devices that do take it.
    F32,
    /// `SND_PCM_FORMAT_S32_LE`. The whole 32-bit container is
    /// significant. What most audiophile USB DACs advertise even when
    /// the converter behind it stops at 24 bits, which costs us
    /// nothing: an `f32` carries 24 bits of mantissa anyway.
    S32,
    /// `SND_PCM_FORMAT_S24_3LE` — 24 bits, three bytes, no container
    /// and so no alignment question. Preferred over [`Self::S24In32`]
    /// for exactly that reason.
    S24Packed,
    /// `SND_PCM_FORMAT_S24_LE`: 24 bits **right-aligned in the low three
    /// bytes** of a 32-bit word.
    ///
    /// This is the exact opposite of WASAPI's `Pcm24Padded`, where
    /// `WAVEFORMATEXTENSIBLE` left-aligns the valid bits and zero-pads
    /// the bottom. The same 24 bits, shifted by 8 in opposite
    /// directions: apply that backend's `<< 8` here and every sample is
    /// multiplied by 256 into permanent clipping; apply this one's
    /// layout there and the signal comes out 48 dB down. Two APIs, two
    /// conventions, one standing temptation to copy the other's packer.
    ///
    /// The top byte carries the sign extension a plain `i32` store
    /// produces. ALSA defines the format as "the low three bytes", so
    /// the driver reads those and what sits above them is ignored.
    S24In32,
    /// `SND_PCM_FORMAT_S16_LE`. Universal last resort.
    S16,
}

impl AlsaSampleFormat {
    /// Bytes this format occupies on the wire, per channel per frame.
    fn bytes_per_sample(self) -> usize {
        match self {
            Self::F32 | Self::S32 | Self::S24In32 => 4,
            Self::S24Packed => 3,
            Self::S16 => 2,
        }
    }

    fn to_alsa(self) -> Format {
        match self {
            Self::F32 => Format::FloatLE,
            Self::S32 => Format::S32LE,
            Self::S24Packed => Format::S243LE,
            Self::S24In32 => Format::S24LE,
            Self::S16 => Format::S16LE,
        }
    }

    /// Label for the diagnostics line, matching ALSA's own spelling so
    /// it can be grepped against `aplay --dump-hw-params`.
    fn label(self) -> &'static str {
        match self {
            Self::F32 => "FLOAT_LE",
            Self::S32 => "S32_LE",
            Self::S24Packed => "S24_3LE",
            Self::S24In32 => "S24_LE",
            Self::S16 => "S16_LE",
        }
    }
}

/// Frames per period we ask ALSA for — about 23 ms at 44.1 kHz.
const TARGET_PERIOD_FRAMES: Frames = 1024;

/// Periods per buffer. Four is the usual choice: deep enough to absorb a
/// scheduling hiccup on a thread that is not realtime, shallow enough
/// that a pause or a seek is heard now rather than after the buffer
/// plays out.
const TARGET_PERIODS: Frames = 4;

/// Ask for a period the ring can feed and a buffer only a few periods
/// deep, on both streams.
///
/// Without this, `HwParams::any` leaves the sizes at whatever the driver
/// offers, and `snd_pcm_hw_params` then takes its maximum. Measured on a
/// `snd-dummy` card: a 16 384-frame period, and a buffer deep enough that
/// starting a track, seeking and changing track each took about ten
/// seconds — the wait was the buffer draining.
///
/// The period also has to stay small against
/// [`super::output::RING_CAPACITY`], because one period is drained from
/// the ring in a single pass and whatever the ring cannot supply is
/// written as silence. At 16 384 frames a period was two thirds of the
/// whole ring, which makes an underrun the normal case rather than the
/// exception.
fn set_period_and_buffer(hwp: &HwParams<'_>) -> AppResult<()> {
    hwp.set_period_size_near(TARGET_PERIOD_FRAMES, ValueOr::Nearest)
        .map_err(|e| AppError::Audio(format!("alsa set_period_size: {e}")))?;
    hwp.set_buffer_size_near(TARGET_PERIOD_FRAMES * TARGET_PERIODS)
        .map_err(|e| AppError::Audio(format!("alsa set_buffer_size: {e}")))?;
    Ok(())
}

/// Pack the mixed `f32` samples into the byte image the negotiated
/// format expects, little-endian throughout.
///
/// Saturation is done by clamping to `[-1.0, 1.0]` before scaling rather
/// than by checking the result: `as` casts on floats saturate in Rust,
/// so an out-of-range sample would land on `i32::MAX` silently instead
/// of at full scale. Gain (volume, normalize, the mono mix) is applied
/// upstream in [`super::output::fill_pcm_period`], so a sample that clips
/// here is one
/// the chain genuinely pushed past 0 dBFS.
///
/// Every path is bounded by both slice lengths, so a short `samples`
/// leaves the tail of `bytes` at whatever it held — the caller keeps one
/// buffer for the life of the stream and rewrites it whole each period.
fn pack_samples(format: AlsaSampleFormat, samples: &[f32], bytes: &mut [u8]) {
    match format {
        AlsaSampleFormat::F32 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
                chunk.copy_from_slice(&sample.to_le_bytes());
            }
        }
        AlsaSampleFormat::S32 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
                // Scaled in f64 because an f32 cannot hold the scale
                // factor: `2_147_483_647.0f32` rounds to 2^31, so every
                // sample would be scaled by a hair too much and full
                // scale would only land right because the cast saturates.
                let v = (f64::from(sample.clamp(-1.0, 1.0)) * 2_147_483_647.0) as i32;
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
        AlsaSampleFormat::S24Packed => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(3)) {
                let v = (sample.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                chunk[0] = (v & 0xFF) as u8;
                chunk[1] = ((v >> 8) & 0xFF) as u8;
                chunk[2] = ((v >> 16) & 0xFF) as u8;
            }
        }
        AlsaSampleFormat::S24In32 => {
            // Right-aligned: the 24-bit value sits in the low three
            // bytes, unshifted. See the variant's doc — this is where
            // WASAPI's `<< 8` does not belong.
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
                let v = (sample.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
        AlsaSampleFormat::S16 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(2)) {
                let v = (sample.clamp(-1.0, 1.0) * 32_767.0) as i16;
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// A device opened and negotiated, with the terms it agreed to.
struct OpenPcm {
    pcm: PCM,
    format: AlsaSampleFormat,
    /// What the device landed on, which is not necessarily what we
    /// asked for — the caller publishes this so the decoder resamples
    /// to it.
    sample_rate: u32,
    channels: u16,
    period_frames: usize,
    /// Logged, not used: it is the number that says how long a pause or
    /// a seek takes to be heard, so a latency report can be read without
    /// asking for another run.
    buffer_frames: Frames,
}

/// Open `dev` for ordinary PCM, walking the format chain until one
/// sticks.
///
/// The rate is a preference here, not a demand ([`ValueOr::Nearest`]):
/// a device that only does 44.1 kHz is still a device we can drive, we
/// just publish 44.1 and let the decoder's resampler meet it. That is
/// the one substantive difference from [`open_dop_pcm`], where a rate
/// the device can't do exactly has to fail the open.
///
/// Both preferences arrive from `SharedPlayback`, where **zero means
/// "no output has ever opened"** rather than a real value — the state
/// starts at 0 and is only filled in by whichever backend opened last.
/// Taken literally, a zero channel count would ask the card for mono
/// on the first launch with exclusive already on, and a card that
/// accepts mono would get it: every track downmixed, for as long as
/// the setting stayed on.
fn open_pcm_negotiated(
    dev: &str,
    preferred_rate: u32,
    preferred_channels: u16,
) -> Result<OpenPcm, PcmOpenError> {
    let rate = match preferred_rate {
        0 => DEFAULT_PCM_RATE,
        rate => rate,
    };
    let mut last: Option<PcmOpenError> = None;
    for channels in channel_candidates(preferred_channels) {
        for format in FORMAT_FALLBACK_CHAIN {
            match try_open_pcm(dev, channels, rate, format) {
                Ok(opened) => return Ok(opened),
                // A card another client holds gives the same answer to
                // every format in the list. Stop and let the caller ask
                // for it through the reservation protocol instead of
                // knocking nine more times.
                Err(failure) if failure.busy => return Err(failure),
                Err(failure) => last = Some(failure),
            }
        }
    }

    Err(last.unwrap_or_else(|| {
        AppError::Audio(format!(
            "alsa: {dev} accepted none of the formats we know how to write"
        ))
        .into()
    }))
}

/// Channel counts to try, in order.
///
/// Stereo closes the list because it is the layout every DAC takes,
/// while asking for the engine's current count first keeps a
/// multichannel output from being quietly folded down to two. Zero is
/// the "nothing has opened an output yet" reading, not a request for
/// no channels — see [`open_pcm_negotiated`].
///
/// Split out from the open so that reading can be checked without a
/// sound card, which is the only place it can be checked at all.
fn channel_candidates(preferred: u16) -> Vec<u16> {
    let mut out = vec![match preferred {
        0 => 2,
        channels => channels,
    }];
    if !out.contains(&2) {
        out.push(2);
    }
    out
}

/// One attempt at one (channels, rate, format) triple. A fresh handle
/// per attempt: a rejected `hw_params` leaves the PCM in a state we would
/// have to reason about, and re-opening a `hw:` device costs microseconds.
fn try_open_pcm(
    dev: &str,
    channels: u16,
    rate: u32,
    format: AlsaSampleFormat,
) -> Result<OpenPcm, PcmOpenError> {
    let pcm = PCM::new(dev, Direction::Playback, false).map_err(|e| PcmOpenError {
        busy: e.errno() == libc::EBUSY,
        err: AppError::Audio(format!("alsa open {dev}: {e}")),
    })?;

    {
        let hwp =
            HwParams::any(&pcm).map_err(|e| AppError::Audio(format!("alsa hwparams: {e}")))?;
        hwp.set_channels(u32::from(channels))
            .map_err(|e| AppError::Audio(format!("alsa set_channels {channels}: {e}")))?;
        hwp.set_rate(rate, ValueOr::Nearest)
            .map_err(|e| AppError::Audio(format!("alsa set_rate {rate}: {e}")))?;
        hwp.set_format(format.to_alsa())
            .map_err(|e| AppError::Audio(format!("alsa set_format {}: {e}", format.label())))?;
        hwp.set_access(Access::RWInterleaved)
            .map_err(|e| AppError::Audio(format!("alsa set_access: {e}")))?;
        set_period_and_buffer(&hwp)?;
        pcm.hw_params(&hwp)
            .map_err(|e| AppError::Audio(format!("alsa hw_params: {e}")))?;
    }

    // Scoped: `hw_params_current` borrows the PCM, and the borrow would
    // otherwise still be live at the `Ok(OpenPcm { pcm, .. })` move.
    let (sample_rate, channels, period_frames, buffer_frames) = {
        let hwp = pcm
            .hw_params_current()
            .map_err(|e| AppError::Audio(format!("alsa hw_params_current: {e}")))?;
        (
            hwp.get_rate()
                .map_err(|e| AppError::Audio(format!("alsa get_rate: {e}")))?,
            hwp.get_channels()
                .map_err(|e| AppError::Audio(format!("alsa get_channels: {e}")))?,
            hwp.get_period_size()
                .map_err(|e| AppError::Audio(format!("alsa get_period_size: {e}")))?
                as usize,
            hwp.get_buffer_size()
                .map_err(|e| AppError::Audio(format!("alsa get_buffer_size: {e}")))?,
        )
    };
    if period_frames == 0 {
        return Err(AppError::Audio("alsa reported a zero period size".into()).into());
    }
    if channels == 0 {
        return Err(AppError::Audio("alsa reported zero channels".into()).into());
    }

    pcm.prepare()
        .map_err(|e| AppError::Audio(format!("alsa prepare: {e}")))?;

    Ok(OpenPcm {
        pcm,
        format,
        sample_rate,
        channels: channels as u16,
        period_frames,
        buffer_frames,
    })
}

/// Why [`write_full_period`] came back.
enum PeriodOutcome {
    /// The whole period reached the device.
    Written,
    /// The engine asked us to stop while we were writing.
    Shutdown,
    /// The device is gone; the string is what to report.
    DeviceLost(String),
}

/// Hand one whole period to the device, re-offering whatever `writei`
/// declined to take.
///
/// The PCM is opened blocking, but `writei` is still allowed to accept
/// fewer frames than offered — on a signal, or after a recovery that
/// swallowed part of the period. The tail has to be re-offered rather
/// than dropped, and only the *unwritten* remainder is re-sent, never
/// frames the device already took.
///
/// `items_per_frame` is the stride of one frame in `buf`'s own unit:
/// the channel count for the DoP path's `i32` words, the frame's byte
/// width for the PCM path's packed bytes.
fn write_full_period<S: Copy>(
    pcm: &PCM,
    io: &IO<'_, S>,
    buf: &[S],
    items_per_frame: usize,
    frames: usize,
    shutdown_rx: &Receiver<()>,
) -> PeriodOutcome {
    let mut frames_done = 0usize;
    while frames_done < frames {
        match io.writei(&buf[frames_done * items_per_frame..]) {
            Ok(0) => {
                // A blocking device that reports no progress and no
                // error has nothing left to recover from.
                return PeriodOutcome::DeviceLost(
                    "alsa accepted 0 frames on a blocking write".into(),
                );
            }
            Ok(n) => frames_done += n,
            Err(err) => {
                if shutdown_rx.try_recv().is_ok() {
                    return PeriodOutcome::Shutdown;
                }
                if let Err(rec) = pcm.try_recover(err, true) {
                    tracing::warn!(?rec, "alsa write failed and recovery failed");
                    return PeriodOutcome::DeviceLost(format!("alsa write failed: {rec}"));
                }
                // Recovered - re-prepare if needed, then retry the tail.
                if pcm.state() == State::Setup {
                    let _ = pcm.prepare();
                }
            }
        }
    }
    PeriodOutcome::Written
}

/// The ordinary-PCM half of the backend: the audiophile path for every
/// track that isn't DSD.
fn pcm_output_thread_main(
    shared: Arc<SharedPlayback>,
    mut consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    init_tx: Sender<AppResult<()>>,
    app: AppHandle,
    device_name: Option<String>,
) {
    let dev = match resolve_hw_device(&device_name) {
        Ok(dev) => dev,
        Err(err) => {
            tracing::warn!(%err, "alsa exclusive: can't map the selected output to a hw: device");
            let _ = init_tx.send(Err(err));
            return;
        }
    };

    // Ask for what the engine is already running at — the cpal default,
    // or whatever a previous output negotiated — so that turning
    // exclusive on doesn't also silently change the resampler's target.
    // Both can still be zero here, meaning nothing has opened an output
    // yet; `open_pcm_negotiated` owns that reading.
    let preferred_rate = shared.sample_rate.load(Ordering::Acquire);
    let preferred_channels = shared.channels.load(Ordering::Acquire);

    let (_reservation, opened) = match open_reserving_the_card(&dev, "pcm", || {
        open_pcm_negotiated(&dev, preferred_rate, preferred_channels)
    }) {
        Ok(opened) => opened,
        Err(err) => {
            tracing::warn!(
                %err,
                device = %dev,
                "alsa exclusive init failed; falling back to shared mode"
            );
            let _ = init_tx.send(Err(err));
            return;
        }
    };
    let OpenPcm {
        pcm,
        format,
        sample_rate,
        channels,
        period_frames,
        buffer_frames,
    } = opened;

    // `io_bytes` rather than a typed `io_*`: the packed image is bytes
    // whatever the negotiated format is, and ALSA derives the frame
    // count from the buffer's byte length against the format it agreed
    // to — so one write path covers all five.
    let io = pcm.io_bytes();

    shared.sample_rate.store(sample_rate, Ordering::Release);
    shared.channels.store(channels, Ordering::Release);
    let _ = init_tx.send(Ok(()));

    let channels = channels as usize;
    let frame_bytes = channels * format.bytes_per_sample();

    tracing::info!(
        device = %dev,
        sample_rate,
        channels,
        format = format.label(),
        period_frames,
        buffer_frames,
        "alsa exclusive stream opened"
    );

    let mut samples: Vec<f32> = vec![0.0; period_frames * channels];
    let mut wire: Vec<u8> = vec![0u8; period_frames * frame_bytes];
    // Every format in the chain is signed and little-endian, and silence
    // in all of them is an all-zero byte pattern — so one pre-zeroed
    // buffer serves the paused and drain paths whichever we landed on.
    let silence: Vec<u8> = vec![0u8; period_frames * frame_bytes];

    let exit = 'run: loop {
        if shutdown_rx.try_recv().is_ok() {
            break ExitReason::Shutdown;
        }

        let out: &[u8] = if shared.paused_output.load(Ordering::Acquire) {
            // Hard pause: write silence so the device doesn't underrun
            // and click, and so the pause is heard now rather than after
            // the pre-buffer drains.
            &silence
        } else if shared.drain_silent.load(Ordering::Acquire) {
            // Drop whatever is queued and emit silence, so the decoder's
            // spin-wait on an empty ring completes within one period.
            while consumer.pop().is_ok() {}
            &silence
        } else {
            let written =
                super::output::fill_pcm_period(&shared, &mut consumer, &mut samples, channels);
            pack_samples(format, &samples, &mut wire);
            if written > 0 {
                shared.samples_played.fetch_add(written, Ordering::Relaxed);
            }
            &wire
        };

        match write_full_period(&pcm, &io, out, frame_bytes, period_frames, &shutdown_rx) {
            PeriodOutcome::Written => {}
            PeriodOutcome::Shutdown => break 'run ExitReason::Shutdown,
            PeriodOutcome::DeviceLost(reason) => break 'run ExitReason::DeviceLost(reason),
        }

        if shutdown_rx.try_recv().is_ok() {
            break ExitReason::Shutdown;
        }
    };

    // `io` borrows `pcm`; both drop here. Dropping the PCM calls
    // `snd_pcm_close`, which releases the exclusive `hw:` handle for the
    // next opener — the sound server included.
    drop(io);

    match exit {
        ExitReason::Shutdown => {
            tracing::debug!("alsa exclusive output thread exiting");
        }
        ExitReason::DeviceLost(reason) => {
            tracing::warn!(
                %reason,
                "alsa exclusive output thread lost the device; requesting rebuild"
            );
            super::output::notify_device_lost(
                &app,
                &shared,
                format!("audio device error: {reason}"),
            );
            super::output::schedule_device_rebuild(&app, super::output::RebuildTarget::Resolve);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::hw_card_index;

    use super::{pack_samples, AlsaSampleFormat};

    #[test]
    fn an_unopened_engine_asks_for_stereo_not_mono() {
        // `SharedPlayback` starts at zero channels and only fills in
        // when a backend opens. Read literally, the first launch with
        // exclusive already on would ask the card for one channel — and
        // a card that grants it would downmix every track from then on.
        assert_eq!(super::channel_candidates(0), vec![2]);
    }

    #[test]
    fn a_known_layout_is_tried_before_stereo_and_stereo_closes_the_list() {
        assert_eq!(super::channel_candidates(6), vec![6, 2]);
        assert_eq!(super::channel_candidates(2), vec![2]);
        // A genuinely mono device keeps its own layout first.
        assert_eq!(super::channel_candidates(1), vec![1, 2]);
    }

    /// The trap this whole variant exists to document. ALSA puts the 24
    /// bits in the LOW three bytes; WASAPI's 24-in-32 puts them in the
    /// HIGH three. Copying that backend's `<< 8` over here would send
    /// every sample out 256x too large.
    #[test]
    fn twenty_four_in_thirty_two_is_right_aligned_not_left() {
        let mut bytes = [0u8; 4];
        pack_samples(AlsaSampleFormat::S24In32, &[1.0], &mut bytes);
        // 8_388_607 = 0x7F_FF_FF, little-endian, top byte untouched.
        assert_eq!(bytes, [0xFF, 0xFF, 0x7F, 0x00]);
        // The left-aligned layout would have been [0x00, 0xFF, 0xFF, 0x7F].
        assert_ne!(bytes, [0x00, 0xFF, 0xFF, 0x7F]);
    }

    #[test]
    fn a_negative_twenty_four_bit_sample_keeps_its_sign_extension() {
        let mut bytes = [0u8; 4];
        pack_samples(AlsaSampleFormat::S24In32, &[-1.0], &mut bytes);
        // -8_388_607 as i32 = 0xFF_80_00_01. The driver reads the low
        // three bytes; the 0xFF above them is the sign extension a plain
        // i32 store produces and is ignored.
        assert_eq!(bytes, [0x01, 0x00, 0x80, 0xFF]);
    }

    #[test]
    fn the_packed_form_spends_three_bytes_and_no_container() {
        let mut bytes = [0u8; 3];
        pack_samples(AlsaSampleFormat::S24Packed, &[1.0], &mut bytes);
        assert_eq!(bytes, [0xFF, 0xFF, 0x7F]);
    }

    #[test]
    fn sixteen_bit_full_scale_lands_on_the_endpoints() {
        let mut bytes = [0u8; 4];
        pack_samples(AlsaSampleFormat::S16, &[1.0, -1.0], &mut bytes);
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), 32_767);
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), -32_767);
    }

    #[test]
    fn a_sample_past_full_scale_saturates_instead_of_wrapping() {
        // Anything above 0 dBFS is clamped before the scale, so it comes
        // out at the endpoint. Wrapping here would turn a loud passage
        // into a burst of full-scale noise of the opposite sign.
        let mut bytes = [0u8; 4];
        pack_samples(AlsaSampleFormat::S32, &[9.0], &mut bytes);
        assert_eq!(i32::from_le_bytes(bytes), 2_147_483_647);
        pack_samples(AlsaSampleFormat::S32, &[-9.0], &mut bytes);
        assert_eq!(i32::from_le_bytes(bytes), -2_147_483_647);
    }

    #[test]
    fn float_output_is_a_byte_copy() {
        let mut bytes = [0u8; 4];
        pack_samples(AlsaSampleFormat::F32, &[0.25], &mut bytes);
        assert_eq!(f32::from_le_bytes(bytes), 0.25);
    }

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
