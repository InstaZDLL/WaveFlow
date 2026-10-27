//! The channel layouts an exclusive open tries, and the order it tries
//! them in.

use wasapi::Device;

use super::LayoutCandidate;

/// Put the track's own rate at the head of the candidate list (#600).
///
/// Every layout the device offered is kept, in its own order, behind a
/// copy of itself at the requested rate — so a device that takes 96 kHz
/// on its stereo layout is asked for that first, and a device that takes
/// none of it still opens at the rate it does offer and lets the
/// decoder's resampler meet it. That fallback is the difference between
/// a preference and DoP's demand.
///
/// Pure so the ordering can be tested without a sound card.
pub(super) fn layouts_at_requested_rate(
    base: &[LayoutCandidate],
    sample_rate: u32,
) -> Vec<LayoutCandidate> {
    // Deduped on the pair the driver is actually asked about, not on the
    // whole candidate: the origin is a label for the log, and a track
    // already at the device's own rate must not have the same pair
    // probed twice — each probe is a COM round trip, and a duplicate is
    // a rejection asked for a second time.
    fn holds(list: &[LayoutCandidate], rate: usize, channels: usize) -> bool {
        list.iter()
            .any(|l| l.sample_rate == rate && l.channels == channels)
    }

    let mut layouts: Vec<LayoutCandidate> = Vec::with_capacity(base.len() * 2);
    for candidate in base {
        if !holds(&layouts, sample_rate as usize, candidate.channels) {
            layouts.push(LayoutCandidate {
                sample_rate: sample_rate as usize,
                channels: candidate.channels,
                origin: "source-rate",
            });
        }
    }
    for candidate in base {
        if !holds(&layouts, candidate.sample_rate, candidate.channels) {
            layouts.push(*candidate);
        }
    }
    layouts
}

/// Build the layout candidates in priority order (#409).
///
/// The order matters, and the first entry is the whole point of this
/// function. `get_mixformat` returns the **shared-mode mix format** —
/// the shape of the pipe into the Windows audio engine, *after* the
/// engine has applied whatever it wants. With "Audio Enhancements",
/// Spatial Sound or a virtual-surround driver in the way, that comes
/// back as 8 channels even though the endpoint is a plain stereo jack.
/// Negotiating exclusive mode against it asks the hardware for a
/// layout it has never supported, so every format in the chain is
/// rejected with `AUDCLNT_E_UNSUPPORTED_FORMAT` (0x88890008) and the
/// user silently drops to shared mode.
///
/// `get_device_format` reads `PKEY_AudioEngine_DeviceFormat` instead —
/// the "Default Format" the user picked in the Windows Sound control
/// panel, which describes the endpoint itself rather than the engine
/// in front of it. That is the layout exclusive mode actually wants.
///
/// The mix format stays as the second candidate (it's correct on the
/// majority of machines, where the two agree anyway), and a plain
/// stereo entry closes the list for drivers that report a
/// multi-channel default no application can open.
pub(super) fn collect_layout_candidates(device: &Device) -> Vec<LayoutCandidate> {
    // A device with no property store, or a driver that doesn't
    // publish the key, is a soft failure — the mix format still gets
    // its turn, which is exactly the pre-#409 behaviour.
    let endpoint = match device.get_device_format() {
        Ok(fmt) => Some((
            fmt.get_samplespersec() as usize,
            fmt.get_nchannels() as usize,
        )),
        Err(err) => {
            tracing::debug!(
                %err,
                "wasapi exclusive: endpoint device format unavailable, falling back to mix format"
            );
            None
        }
    };

    let mix = match device
        .get_iaudioclient()
        .and_then(|client| client.get_mixformat())
    {
        Ok(fmt) => Some((
            fmt.get_samplespersec() as usize,
            fmt.get_nchannels() as usize,
        )),
        Err(err) => {
            tracing::debug!(%err, "wasapi exclusive: mix format unavailable");
            None
        }
    };

    build_layout_candidates(endpoint, mix)
}

/// Ordering + dedupe half of [`collect_layout_candidates`], split out
/// so it can be unit-tested without a COM apartment or real hardware.
///
/// Both inputs are `(sample_rate, channels)`, and either may be absent
/// when the corresponding query failed.
fn build_layout_candidates(
    endpoint: Option<(usize, usize)>,
    mix: Option<(usize, usize)>,
) -> Vec<LayoutCandidate> {
    // Dedupe on the layout, not the origin: the endpoint format and
    // the mix format agree on most machines, and probing the same
    // pair twice would just double the failure log. A zero in either
    // field means the driver handed back a malformed format — skip it
    // rather than ask WASAPI for a 0-channel stream.
    fn push(
        candidates: &mut Vec<LayoutCandidate>,
        sample_rate: usize,
        channels: usize,
        origin: &'static str,
    ) {
        if channels > 0
            && sample_rate > 0
            && !candidates
                .iter()
                .any(|c| c.sample_rate == sample_rate && c.channels == channels)
        {
            candidates.push(LayoutCandidate {
                sample_rate,
                channels,
                origin,
            });
        }
    }

    let mut candidates: Vec<LayoutCandidate> = Vec::new();

    if let Some((rate, channels)) = endpoint {
        push(&mut candidates, rate, channels, "endpoint");
    }

    if let Some((rate, channels)) = mix {
        push(&mut candidates, rate, channels, "mix");
        // Channel-axis fallback: same rate, plain stereo. Catches the
        // case where both reported layouts are inflated.
        push(&mut candidates, rate, 2, "stereo");
    }

    // Stereo at the endpoint rate too, in case the endpoint and the
    // engine disagree on the rate as well as the channel count.
    if let Some((rate, _)) = endpoint {
        push(&mut candidates, rate, 2, "stereo");
    }

    candidates
}

#[cfg(test)]
mod tests;
