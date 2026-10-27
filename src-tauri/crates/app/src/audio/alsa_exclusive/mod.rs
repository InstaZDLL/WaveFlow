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

use alsa::pcm::{Format, Frames, State, IO, PCM};
use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use tauri::AppHandle;

use super::output::{DopFormat, OutputHandle, RequestedFormat, RING_CAPACITY};
use super::pcm_scale;
use super::state::SharedPlayback;
use crate::error::{AppError, AppResult};

mod device;
mod open;

pub(super) use device::probe_capabilities;
pub use open::refusal_of;

use device::resolve_hw_device;
use open::{open_dop_pcm, open_pcm_negotiated, open_reserving_the_card};

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
    requested: Option<RequestedFormat>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<()>>(1);

    let thread_shared = shared.clone();
    let thread_app = app.clone();
    let thread_device = device_name.clone();
    let join: JoinHandle<()> = std::thread::Builder::new()
        .name(match requested {
            Some(r) if r.dop => "waveflow-alsa-dop".into(),
            _ => "waveflow-alsa-exclusive".to_string(),
        })
        .spawn(move || match requested.and_then(RequestedFormat::as_dop) {
            Some(dop) => output_thread_main(
                thread_shared,
                consumer,
                shutdown_rx,
                init_tx,
                thread_app,
                thread_device,
                dop,
            ),
            // #600: a rate request rides into the PCM path as the rate to
            // ask the card for, ahead of whatever the last stream opened
            // at.
            None => pcm_output_thread_main(
                thread_shared,
                consumer,
                shutdown_rx,
                init_tx,
                thread_app,
                thread_device,
                requested.map(|r| r.sample_rate),
            ),
        })
        .map_err(|e| AppError::Audio(format!("spawn alsa exclusive thread: {e}")))?;

    match init_rx.recv() {
        Ok(Ok(())) => Ok((
            producer,
            OutputHandle {
                shutdown_tx,
                join,
                // `resolve_hw_device` either places the pinned name on a
                // real card or fails outright — it never quietly takes a
                // different one — so a pin that reached this point is
                // what plays. An absent pin lands on card 0, which has no
                // name in the picker's terms, so this reports unknown
                // instead of guessing one (#612).
                opened_device: device_name.clone(),
                device_name,
                // We hold the card through a raw `hw:` handle: nothing
                // else can mix into it while this thread lives. That is
                // true of the DoP stream too — it used to report `false`
                // here, which made the pipeline panel deny an exclusive
                // grab that had in fact happened (WASAPI has always
                // reported `true` for both).
                exclusive: true,
                // A rate request is not DoP however exact it lands: the
                // handle records the packing, not the accuracy.
                dop: requested.and_then(RequestedFormat::as_dop),
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

    // What the output was running at before DoP took over. The PCM
    // half reads `sample_rate` / `channels` as its *preference* for the
    // next open, and a DoP rate is not one: a DAC that does DSD128
    // usually also accepts 352.8 kHz as PCM, so the track after the DSD
    // one would open there and have rubato upsample every 44.1 kHz
    // source eightfold for nothing. Put back what we found on the way
    // out — see the restore at the end of this function.
    let pre_dop_rate = shared.sample_rate.load(Ordering::Acquire);
    let pre_dop_channels = shared.channels.load(Ordering::Acquire);

    shared.sample_rate.store(dop.sample_rate, Ordering::Release);
    shared.channels.store(dop.channels, Ordering::Release);
    shared.set_sample_format(super::state::OutputSampleFormat::Dop);
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

    // Hand the DoP rate back before anyone can mistake it for a PCM
    // preference. `OutputHandle::stop` joins this thread, so every
    // deliberate teardown has published this before the replacement
    // output opens; on a device loss the rebuild is scheduled from
    // below, after the same store. Whatever opens next overwrites both
    // values with what it actually negotiated — this only decides what
    // that open *asks* for.
    shared.sample_rate.store(pre_dop_rate, Ordering::Release);
    shared.channels.store(pre_dop_channels, Ordering::Release);

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

    /// What the rest of the engine calls this format.
    fn sample_format(self) -> super::state::OutputSampleFormat {
        use super::state::OutputSampleFormat as Out;
        match self {
            Self::F32 => Out::F32,
            Self::S32 => Out::S32,
            Self::S24Packed => Out::S24,
            Self::S24In32 => Out::S24In32,
            Self::S16 => Out::S16,
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

/// Pack the mixed `f32` samples into the byte image the negotiated
/// format expects, little-endian throughout.
///
/// The integer formats go through [`super::pcm_scale`], which inverts the
/// decoder's own float conversion exactly, so an untouched stream reaches
/// the device bit for bit, and a sample past full scale saturates at the
/// endpoint rather than wrapping. 16-bit output is dithered where the
/// chain moved a sample off a 16-bit step, and only there
/// ([`pcm_scale::Dither16`]). Gain (volume, normalize) is applied
/// upstream in [`super::output::fill_pcm_period`], so a sample that clips
/// here is one
/// the chain genuinely pushed past 0 dBFS.
///
/// Every path is bounded by both slice lengths, so a short `samples`
/// leaves the tail of `bytes` at whatever it held — the caller keeps one
/// buffer for the life of the stream and rewrites it whole each period.
fn pack_samples(
    format: AlsaSampleFormat,
    samples: &[f32],
    bytes: &mut [u8],
    dither: &mut pcm_scale::Dither16,
) {
    match format {
        AlsaSampleFormat::F32 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
                chunk.copy_from_slice(&sample.to_le_bytes());
            }
        }
        AlsaSampleFormat::S32 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
                let v = pcm_scale::to_i32(*sample);
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
        AlsaSampleFormat::S24Packed => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(3)) {
                let v = pcm_scale::to_i24(*sample);
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
                let v = pcm_scale::to_i24(*sample);
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
        AlsaSampleFormat::S16 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(2)) {
                let v = dither.quantize(*sample);
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
    // `requested_rate`: the rate the track being loaded is in, when the
    // user asked us to open at it (#600). A preference, not a demand —
    // `set_rate` is asked with `ValueOr::Nearest`, so a card that cannot
    // do it opens at what it can and the decoder's resampler meets that.
    requested_rate: Option<u32>,
) {
    let dev = match resolve_hw_device(&device_name) {
        Ok(dev) => dev,
        Err(err) => {
            tracing::warn!(%err, "alsa exclusive: can't map the selected output to a hw: device");
            let _ = init_tx.send(Err(err));
            return;
        }
    };

    // The track's own rate when one was asked for (#600), and otherwise
    // what the engine is already running at — the cpal default, or
    // whatever a previous output negotiated — so that turning exclusive
    // on doesn't also silently change the resampler's target. Both can
    // still be zero here, meaning nothing has opened an output yet;
    // `open_pcm_negotiated` owns that reading.
    let preferred_rate =
        requested_rate.unwrap_or_else(|| shared.sample_rate.load(Ordering::Acquire));
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
    shared.set_sample_format(format.sample_format());
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
    let mut dither = pcm_scale::Dither16::new();

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
            pack_samples(format, &samples, &mut wire, &mut dither);
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
mod tests;
