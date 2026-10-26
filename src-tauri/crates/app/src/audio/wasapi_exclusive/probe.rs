//! What the driver accepts in exclusive mode, asked without opening a stream.

use crate::error::{AppError, AppResult};

use super::layouts::collect_layout_candidates;
use super::session::pick_device;
use super::{ComApartment, ExclusiveSampleFormat, FORMAT_FALLBACK_CHAIN};

/// Ask the driver what it accepts **in exclusive mode** (#593).
///
/// The strongest of the three capability probes, because it is the same
/// call the real open makes: `is_supported_exclusive_with_quirks` offers
/// each (format, rate) pair to the driver and reports what came back,
/// working around the `WAVEFORMATEXTENSIBLE` and channel-mask quirks that
/// otherwise hide behind a generic unsupported-format error (#405). No
/// stream is initialised and nothing is opened, so this is safe to run
/// while the device is playing — including while *we* hold it
/// exclusively.
///
/// Blocking, and not cheap: one COM round trip per pair. It belongs
/// behind [`crate::audio::capabilities::probe_output_device`], which asks once
/// per device and remembers the answer.
pub(in crate::audio) fn probe_capabilities(
    device_name: Option<&str>,
) -> AppResult<crate::audio::capabilities::DeviceCapabilities> {
    use crate::audio::capabilities::{
        CapabilitySource, DeviceCapabilities, DeviceFormat, PROBE_RATES,
    };
    use std::collections::BTreeSet;

    // The probe can run on any thread the command hands it, so it
    // initialises COM like the render thread does. Already-initialised is
    // not an error.
    wasapi::initialize_mta()
        .ok()
        .map_err(|e| AppError::Audio(format!("CoInitializeEx(MTA) failed: {e:?}")))?;
    // Declared *before* every COM object below, so that it drops last:
    // locals are destroyed in reverse declaration order, and releasing an
    // interface after `CoUninitialize` is undefined behaviour.
    let _com = ComApartment;

    let requested = device_name.map(str::to_string);
    let device = pick_device(&requested)?;
    // `pick_device` falls back to the default endpoint when the name it
    // was given is no longer enumerated. Describing that endpoint under
    // the requested name would be a sheet about the wrong hardware, and
    // the answer is memoised for the session — so the mismatch is an
    // error rather than a silent substitution.
    if let Some(name) = requested.as_deref().filter(|name| !name.is_empty()) {
        let opened = device.get_friendlyname().ok();
        if opened.as_deref() != Some(name) {
            return Err(AppError::Audio(format!(
                "the requested device is no longer present (opened {} instead)",
                opened.as_deref().unwrap_or("<unnamed>")
            )));
        }
    }
    let client = device
        .get_iaudioclient()
        .map_err(|e| AppError::Audio(format!("get IAudioClient: {e:?}")))?;
    // The mix format is the device's own idea of its shape: it is where
    // the channel count comes from, and probing at a channel count the
    // endpoint does not have would fail every pair for the wrong reason.
    let mix = client
        .get_mixformat()
        .map_err(|e| AppError::Audio(format!("get mix format: {e:?}")))?;
    let channels = mix.get_nchannels();
    // The mix format's channel count is what the *probe* asks at — the
    // layout exclusive mode actually wants (#409) — but it is not a
    // maximum: an eight-channel endpoint whose Windows default format is
    // stereo reports two. The sheet says "max channels", so it takes the
    // widest layout the device reports for itself, which is what the two
    // other backends answer with.
    let max_channels = collect_layout_candidates(&device)
        .iter()
        .map(|layout| layout.channels as u16)
        .chain(std::iter::once(channels))
        .max()
        .unwrap_or(channels);
    let device_period = client.get_device_period().ok();

    let mut formats = Vec::new();
    let mut rates: BTreeSet<u32> = BTreeSet::new();
    for format in FORMAT_FALLBACK_CHAIN {
        // Kept per format, not merged into the set below: a DAC that
        // takes 32-bit to 96 kHz and 24-bit to 192 kHz accepts neither
        // pair the two maxima would suggest.
        let mut accepted_rates = Vec::new();
        for &rate in PROBE_RATES {
            let wave = format.to_wave_format(rate as usize, channels as usize);
            // A fresh client per probe, for the same reason the open path
            // takes one: a client that has been refused is not reliably
            // reusable, and some drivers leave it in a state where every
            // later question answers no.
            let Ok(probe) = device.get_iaudioclient() else {
                continue;
            };
            if probe.is_supported_exclusive_with_quirks(&wave).is_ok() {
                accepted_rates.push(rate);
                rates.insert(rate);
            }
        }
        if !accepted_rates.is_empty() {
            let (bits, float) = match format {
                ExclusiveSampleFormat::Float32 => (32, true),
                // 24 in both layouts: the padding byte of `S24_4LE` is
                // not resolution.
                ExclusiveSampleFormat::Pcm24Packed | ExclusiveSampleFormat::Pcm24Padded => {
                    (24, false)
                }
                ExclusiveSampleFormat::Pcm16 => (16, false),
            };
            formats.push(DeviceFormat {
                label: format.label().to_string(),
                bits,
                float,
                sample_rates: accepted_rates,
            });
        }
    }

    // The **minimum** period, not the default one: that is the quantity
    // every backend can answer, so it is the one the sheet shows (see
    // `DeviceCapabilities::min_period_frames`). It comes back in
    // 100-nanosecond units; frames are what a buffer means to anyone
    // reading it, at the device's own mix rate — the rate that period
    // was measured for.
    let min_period_frames = device_period.map(|(_default, min_period)| {
        let rate = mix.get_samplespersec() as i64;
        ((min_period.max(0) * rate) / 10_000_000) as u32
    });

    Ok(DeviceCapabilities {
        device_id: requested,
        source: CapabilitySource::WasapiExclusive,
        formats,
        sample_rates: rates.into_iter().collect(),
        max_channels,
        min_period_frames,
        unavailable_reason: None,
    })
}
