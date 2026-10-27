//! Opening the device in exclusive mode: the format negotiation and the
//! stream initialisation it ends with.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use wasapi::{
    AudioClient, AudioRenderClient, Device, DeviceEnumerator, Direction, Handle, StreamMode,
    WaveFormat,
};

use crate::audio::output::RequestedFormat;
use crate::audio::state::SharedPlayback;
use crate::error::{AppError, AppResult};

use super::layouts::{collect_layout_candidates, layouts_at_requested_rate};
use super::{
    refusal_of, ExclusiveSampleFormat, ExclusiveSession, LayoutCandidate, DOP_FORMAT_CHAIN,
    FORMAT_FALLBACK_CHAIN,
};

/// Resolve the device, then walk every (layout × bit depth) candidate
/// (#174, #409). Layouts come from [`collect_layout_candidates`] —
/// endpoint format first, then the mix format, then plain stereo —
/// and each one is probed against the full format chain before moving
/// on. For each candidate we acquire a fresh `IAudioClient`,
/// check the format is supported in exclusive mode, and try to
/// initialize. Many consumer DACs (Realtek ALC, Conexant CX, some
/// USB codecs) reject `IEEE_FLOAT` outright with
/// `AUDCLNT_E_UNSUPPORTED_FORMAT` (0x88890008) but accept PCM at
/// 24-bit or 16-bit — without this chain those users got bumped to
/// cpal shared mode and never reached bit-perfect playback even
/// with the WASAPI Exclusive toggle on.
///
/// A fresh `IAudioClient` per attempt is deliberate: once an
/// `IAudioClient` has been initialized it can't be re-initialized,
/// and an `is_supported` rejection still leaves the COM object in
/// a "you initialized me with the wrong shape" state on some
/// drivers. Acquiring a new one is cheap and reliable.
pub(super) fn open_exclusive_session(
    device_name: &Option<String>,
    shared: &Arc<SharedPlayback>,
    requested: Option<RequestedFormat>,
) -> AppResult<ExclusiveSession> {
    let device = pick_device(device_name)?;
    // Read the name back off the endpoint we were handed: this is the
    // only place that knows whether the fallback inside `pick_device`
    // fired (#612).
    let opened_device = device.get_friendlyname().ok();

    // DoP (#495) pins both axes: the layout is the exact DoP format
    // (`dsd_rate / 16`, source channels) and the bit-depth chain is
    // 24-bit only. A DoP payload lives in 24 bits — Float32 would
    // reinterpret the marker bytes as an exponent and 16-bit would
    // truncate the low DSD byte, so neither can carry it. If the DAC
    // refuses 24-bit at the DoP rate the whole open fails and the
    // caller falls back to DSD → PCM (it never drops to shared mode).
    let (layouts, formats): (Vec<LayoutCandidate>, &[ExclusiveSampleFormat]) = match requested {
        Some(r) if r.dop => (
            vec![LayoutCandidate {
                sample_rate: r.sample_rate as usize,
                channels: r.channels.unwrap_or(2) as usize,
                origin: "dop",
            }],
            &DOP_FORMAT_CHAIN,
        ),
        other => {
            let base = collect_layout_candidates(&device);
            if base.is_empty() {
                return Err(AppError::Audio(
                    "wasapi exclusive: device reported no usable sample rate / channel layout"
                        .into(),
                ));
            }
            // #600: the track's own rate first, on each layout the device
            // offered, with the device's own rates kept behind it. A
            // driver that refuses the source rate therefore falls back
            // rather than failing, which is what makes this a preference.
            let layouts = match other {
                Some(r) => layouts_at_requested_rate(&base, r.sample_rate),
                None => base,
            };
            (layouts, &FORMAT_FALLBACK_CHAIN)
        }
    };

    let mut last_err: Option<AppError> = None;
    // The most telling cause across every attempt, not the last one's: a
    // device in use can be refused as busy on one layout and on format on
    // the next, and "close the other app" is what the user needs to read.
    let mut reason: Option<crate::audio::engine::ExclusiveRefusal> = None;
    // Every rejection, kept for the summary below. `debug` alone was
    // not enough: release logs run at `info`, so a user reporting a
    // failure only ever showed us the last attempt of eight and the
    // other seven had to be guessed at.
    let mut failures: Vec<String> = Vec::new();
    for layout in &layouts {
        for &format in formats.iter() {
            match try_open_with_format(&device, format, layout.sample_rate, layout.channels) {
                Ok((client, render, event, buffer_frames)) => {
                    tracing::info!(
                        sample_rate = layout.sample_rate as u32,
                        channels = layout.channels as u16,
                        format = format.label(),
                        layout = layout.origin,
                        "wasapi exclusive negotiation succeeded"
                    );
                    shared
                        .sample_rate
                        .store(layout.sample_rate as u32, Ordering::Release);
                    shared
                        .channels
                        .store(layout.channels as u16, Ordering::Release);
                    shared.set_sample_format(if requested.is_some_and(|r| r.dop) {
                        crate::audio::state::OutputSampleFormat::Dop
                    } else {
                        format.sample_format()
                    });
                    return Ok(ExclusiveSession {
                        client,
                        render,
                        event,
                        opened_device,
                        sample_rate: layout.sample_rate as u32,
                        channels: layout.channels as u16,
                        buffer_frames,
                        format,
                        dop: requested.is_some_and(|r| r.dop),
                    });
                }
                Err(err) => {
                    // Log the full triple, not just the bit depth: a
                    // rejection blamed on the format is very often the
                    // channel count instead, and the pre-#409 log made
                    // that impossible to tell apart from a diagnostics
                    // dump.
                    tracing::debug!(
                        sample_rate = layout.sample_rate as u32,
                        channels = layout.channels as u16,
                        format = format.label(),
                        layout = layout.origin,
                        %err,
                        "wasapi exclusive candidate rejected; trying next"
                    );
                    failures.push(format!(
                        "{}@{}Hz/{}ch[{}]: {err}",
                        format.label(),
                        layout.sample_rate,
                        layout.channels,
                        layout.origin
                    ));
                    let this = refusal_of(&err);
                    reason = Some(match reason {
                        Some(seen) if seen.outranks(this) => seen,
                        _ => this,
                    });
                    last_err = Some(err);
                }
            }
        }
    }

    tracing::warn!(
        candidates = ?layouts,
        attempts = failures.len(),
        failures = %failures.join(" | "),
        "wasapi exclusive: no (rate, channels, format) combination was accepted"
    );

    let err = last_err.unwrap_or_else(|| {
        AppError::Audio("wasapi exclusive: every format in the fallback chain was rejected".into())
    });
    Err(match reason {
        Some(reason) => reason.refuse(err),
        None => err,
    })
}

/// Do two `WaveFormat`s describe the same stream? Compares every
/// field that matters to the driver, including the container tag
/// (`cbSize` distinguishes `WAVEFORMATEX` from `WAVEFORMATEXTENSIBLE`)
/// and the channel mask.
///
/// Used only to skip a redundant second init attempt, never to decide
/// whether a format is acceptable.
///
/// Deliberately does **not** compare the subformat GUID (Int vs Float):
/// both operands always come from the same [`ExclusiveSampleFormat`],
/// and the quirk probe only ever changes the channel mask or the
/// `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE` shape. Don't reuse this as a
/// general format equality without adding that field.
fn same_wave_format(a: &WaveFormat, b: &WaveFormat) -> bool {
    a.wave_fmt.Format.cbSize == b.wave_fmt.Format.cbSize
        && a.get_nchannels() == b.get_nchannels()
        && a.get_samplespersec() == b.get_samplespersec()
        && a.get_bitspersample() == b.get_bitspersample()
        && a.get_validbitspersample() == b.get_validbitspersample()
        && a.get_dwchannelmask() == b.get_dwchannelmask()
}

/// Single attempt with one bit-depth format. Probes the format, then
/// initializes + starts the stream on a fresh `IAudioClient`.
///
/// The probe goes through `is_supported_exclusive_with_quirks` rather
/// than a bare `is_supported` (#405). `WaveFormat::new` always builds a
/// `WAVEFORMATEXTENSIBLE`, and a good number of drivers reject that
/// representation outright with `AUDCLNT_E_UNSUPPORTED_FORMAT` while
/// happily accepting the *same* PCM layout described as a plain
/// `WAVEFORMATEX` — which is why a device can turn down 16-bit stereo
/// at its own reported rate, the most universally supported format
/// there is. The helper also re-probes against each recommended
/// `ksmedia.h` channel mask, since a multi-channel endpoint typically
/// accepts exactly one and `WaveFormat::new(.., None)` guesses.
fn try_open_with_format(
    device: &Device,
    format: ExclusiveSampleFormat,
    sample_rate: usize,
    channels: usize,
) -> AppResult<(AudioClient, AudioRenderClient, Handle, u32)> {
    let requested = format.to_wave_format(sample_rate, channels);

    let probe_client = device
        .get_iaudioclient()
        .map_err(|e| AppError::Audio(format!("get IAudioClient ({}): {e:?}", format.label())))?;
    let accepted = probe_client
        .is_supported_exclusive_with_quirks(&requested)
        .map_err(|e| {
            AppError::Audio(format!(
                "is_supported rejected {} in exclusive mode: {e:?}",
                format.label()
            ))
        })?;
    drop(probe_client);

    // wasapi's own docs warn that some drivers validate the simplified
    // representation but want the original one at init time. So try
    // the shape the probe accepted first, then fall back to the shape
    // we asked for — but only when they actually differ, otherwise the
    // retry is a guaranteed repeat of the same failure.
    match init_stream(device, format, channels, &accepted) {
        Ok(session) => Ok(session),
        Err(accepted_err) if !same_wave_format(&accepted, &requested) => {
            tracing::debug!(
                format = format.label(),
                err = %accepted_err,
                "wasapi exclusive: init with the probe-accepted shape failed, retrying as requested"
            );
            // Carry both failures. The first one only ever went to
            // `debug`, which release builds don't emit — the same blind
            // spot that made this bug take three passes to find. When
            // both shapes are refused, the caller's summary must say so.
            init_stream(device, format, channels, &requested).map_err(|requested_err| {
                AppError::Audio(format!(
                    "{} refused in both shapes — probe-accepted: {accepted_err}; as requested: {requested_err}",
                    format.label()
                ))
            })
        }
        Err(err) => Err(err),
    }
}

/// Initialize + start an exclusive stream with one concrete
/// `WaveFormat`. Pre-fills with silence sized to that format so the
/// first device event lands on clean state.
///
/// Always takes a fresh `IAudioClient`: once initialized (or once an
/// init has been refused) a client can't be reused, and some drivers
/// leave a rejected one in an unusable state.
fn init_stream(
    device: &Device,
    format: ExclusiveSampleFormat,
    channels: usize,
    wave: &WaveFormat,
) -> AppResult<(AudioClient, AudioRenderClient, Handle, u32)> {
    let mut client = device
        .get_iaudioclient()
        .map_err(|e| AppError::Audio(format!("get IAudioClient ({}): {e:?}", format.label())))?;

    let (default_period, min_period) = client
        .get_device_period()
        .map_err(|e| AppError::Audio(format!("get_device_period: {e:?}")))?;
    let period_hns = min_period.max(default_period);
    let mode = StreamMode::EventsExclusive { period_hns };

    client
        .initialize_client(wave, &Direction::Render, &mode)
        .map_err(|e| {
            AppError::Audio(format!(
                "initialize_client {} exclusive: {e:?}",
                format.label()
            ))
        })?;

    let event = client
        .set_get_eventhandle()
        .map_err(|e| AppError::Audio(format!("set_get_eventhandle: {e:?}")))?;

    let buffer_frames = client
        .get_buffer_size()
        .map_err(|e| AppError::Audio(format!("get_buffer_size: {e:?}")))?;

    let render = client
        .get_audiorenderclient()
        .map_err(|e| AppError::Audio(format!("get_audiorenderclient: {e:?}")))?;

    let silent_bytes = vec![0u8; (buffer_frames as usize) * channels * format.bytes_per_sample()];
    render
        .write_to_device(buffer_frames as usize, &silent_bytes, None)
        .map_err(|e| AppError::Audio(format!("prefill render buffer: {e:?}")))?;

    client
        .start_stream()
        .map_err(|e| AppError::Audio(format!("start_stream: {e:?}")))?;

    Ok((client, render, event, buffer_frames))
}

pub(super) fn pick_device(device_name: &Option<String>) -> AppResult<Device> {
    // wasapi 0.23 removed the free `get_default_device` / `DeviceCollection`
    // entry points; everything now goes through a `DeviceEnumerator`
    // (which owns the underlying `IMMDeviceEnumerator` COM pointer).
    let enumerator = DeviceEnumerator::new()
        .map_err(|e| AppError::Audio(format!("DeviceEnumerator::new: {e:?}")))?;
    if let Some(name) = device_name.as_deref().filter(|n| !n.is_empty()) {
        // Friendly-name lookup lives on the collection, not the
        // enumerator (the enumerator's `get_device` wants an opaque
        // device-id, not the human-readable name we persist).
        let coll = enumerator
            .get_device_collection(&Direction::Render)
            .map_err(|e| AppError::Audio(format!("enumerate render devices: {e:?}")))?;
        if let Ok(dev) = coll.get_device_with_name(name) {
            return Ok(dev);
        }
        tracing::warn!(
            requested = name,
            "wasapi exclusive: requested device not found, falling back to default"
        );
    }
    enumerator
        .get_default_device(&Direction::Render)
        .map_err(|e| AppError::Audio(format!("default render device: {e:?}")))
}

#[cfg(test)]
mod tests;
