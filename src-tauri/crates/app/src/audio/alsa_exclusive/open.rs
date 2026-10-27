//! Opening the PCM: taking the card from the sound server, the DoP and
//! negotiated PCM opens, and why an open was refused.

use std::time::{Duration, Instant};

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};

use crate::error::{AppError, AppResult};

use super::super::output::DopFormat;
use super::device::hw_card_index;
use super::{
    AlsaSampleFormat, OpenPcm, PcmOpenError, DEFAULT_PCM_RATE, FORMAT_FALLBACK_CHAIN,
    TARGET_PERIODS, TARGET_PERIOD_FRAMES,
};

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
pub(super) fn open_reserving_the_card<T>(
    dev: &str,
    what: &str,
    mut attempt: impl FnMut() -> Result<T, PcmOpenError>,
) -> Result<(Option<super::super::device_reservation::Reservation>, T), AppError> {
    match attempt() {
        Ok(opened) => Ok((None, opened)),
        Err(failure) if failure.busy => {
            let Some(reservation) =
                hw_card_index(dev).and_then(super::super::device_reservation::Reservation::acquire)
            else {
                tracing::warn!(
                    device = %dev,
                    %what,
                    "alsa: the card is busy and could not be reserved"
                );
                return Err(busy(failure.err));
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
                    if failure.busy {
                        Err(busy(failure.err))
                    } else {
                        Err(failure.err)
                    }
                }
            }
        }
        Err(failure) => Err(failure.err),
    }
}

/// The device stayed held by someone else: a refusal that says so.
fn busy(err: AppError) -> AppError {
    super::super::engine::ExclusiveRefusal::Busy.refuse(err)
}

/// Why an exclusive open failed. Busy is marked where it is known; a
/// rejected hardware parameter (rate, sample format, channels) means the
/// device offers no format the stream could use.
pub fn refusal_of(err: &AppError) -> super::super::engine::ExclusiveRefusal {
    use super::super::engine::ExclusiveRefusal;
    match err {
        AppError::ExclusiveRefused { reason, .. } => *reason,
        AppError::Audio(text)
            if text.starts_with("alsa set_")
                || text.starts_with("alsa hw_params")
                || text.starts_with("alsa hwparams") =>
        {
            ExclusiveRefusal::Format
        }
        _ => ExclusiveRefusal::Unknown,
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
    let deadline = Instant::now() + super::super::device_reservation::RELEASE_GRACE;
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
pub(super) fn open_dop_pcm(dev: &str, dop: DopFormat) -> Result<(PCM, usize), PcmOpenError> {
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
/// [`super::super::output::RING_CAPACITY`], because one period is drained from
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
pub(super) fn open_pcm_negotiated(
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
pub(super) fn channel_candidates(preferred: u16) -> Vec<u16> {
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
