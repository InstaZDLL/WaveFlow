//! Which card a pinned output name reaches, and what that card can play.

use alsa::pcm::{HwParams, PCM};
use alsa::Direction;

use crate::error::{AppError, AppResult};

use super::{AlsaSampleFormat, FORMAT_FALLBACK_CHAIN};

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
pub(super) fn resolve_hw_device(name: &Option<String>) -> AppResult<String> {
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
    // No selector: one of the system's routes, which is "the default
    // output" by another name — the same reading as no selection — or a
    // card's friendly name.
    if is_system_route(n) {
        return Ok("hw:0,0".to_string());
    }
    if let Some(index) = find_card_index(n) {
        return Ok(format!("hw:{index},0"));
    }
    Err(AppError::Audio(format!(
        "alsa: no card matches the selected output '{n}' — can't open it exclusively"
    )))
}

/// Whether `name` is one of the routes into the system's sound server
/// rather than a card: `default`, and the `pipewire` / `pulse` devices
/// PipeWire and PulseAudio register in ALSA. cpal lists them next to the
/// cards, so they can be pinned like one — "PipeWire Sound Server" is
/// what a PipeWire desktop offers first. Treating only `default` as a
/// route refused a pinned `pipewire` outright: every rebuild then failed
/// back to shared mode while the launch, reading no pin, opened the card.
pub(super) fn is_system_route(name: &str) -> bool {
    ["default", "pipewire", "pulse"]
        .iter()
        .any(|route| name.eq_ignore_ascii_case(route))
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
pub(super) fn hw_card_index(dev: &str) -> Option<i32> {
    let first = dev.strip_prefix("hw:")?.split(',').next()?;
    if let Ok(index) = first.parse::<i32>() {
        return Some(index);
    }
    find_card_index(first.strip_prefix("CARD=")?)
}

/// Ask the hardware itself what it accepts (#593).
///
/// Asked of the raw `hw:` device, so the answer describes the hardware
/// with no plug layer in between — which is the whole reason exclusive
/// output targets `hw:` in the first place. A device another client is
/// holding cannot answer at all, and says so rather than guessing.
///
/// Opened **non-blocking**: a busy device must come back immediately with
/// `EBUSY`, not park the caller on an open that waits for the other
/// client to finish. Nothing is configured and nothing is written — the
/// PCM is closed as soon as the parameters have been read.
pub(in crate::audio) fn probe_capabilities(
    device_name: Option<&str>,
) -> AppResult<super::super::capabilities::DeviceCapabilities> {
    use super::super::capabilities::{
        CapabilitySource, DeviceCapabilities, DeviceFormat, PROBE_RATES,
    };
    use std::collections::BTreeSet;

    let requested = device_name.map(str::to_string);
    // `resolve_hw_device` answers `hw:0,0` for both no name and the name
    // "default" — a sound choice when something has to be *opened*, and
    // the wrong one for a sheet: card 0 is not what `default` reaches,
    // which is whatever the plug layer routes to, and that can be a
    // sound server feeding another card entirely. Describing card 0
    // under the name "default" would be a sheet about hardware the user
    // may never hear, and it is memoised for the session. The listing
    // keeps a `default` row on purpose (#594), so this is reachable from
    // the menu, not just from an unpinned call. The two other backends
    // already refuse their own fallback for the same reason.
    let describes_hardware = requested
        .as_deref()
        .filter(|n| !n.is_empty())
        .is_some_and(|n| !is_system_route(n));
    if !describes_hardware {
        return Err(AppError::Audio(
            "no pinned ALSA card to describe — a system route is not hardware".to_string(),
        ));
    }
    let hw = resolve_hw_device(&requested)?;
    let pcm = PCM::new(&hw, Direction::Playback, true)
        .map_err(|e| AppError::Audio(format!("open {hw} to read its capabilities: {e}")))?;
    let hwp = HwParams::any(&pcm)
        .map_err(|e| AppError::Audio(format!("read {hw} hardware parameters: {e}")))?;

    // One parameter set per format, and the rates asked of *that* set:
    // narrowing the space to a format first is what makes the answer a
    // pair the card really accepts, rather than two independent maxima
    // that suggest one it never has.
    let mut formats: Vec<DeviceFormat> = Vec::new();
    let mut sample_rates: BTreeSet<u32> = BTreeSet::new();
    for format in FORMAT_FALLBACK_CHAIN {
        let Ok(params) = HwParams::any(&pcm) else {
            continue;
        };
        if params.set_format(format.to_alsa()).is_err() {
            continue;
        }
        let accepted: Vec<u32> = PROBE_RATES
            .iter()
            .copied()
            .filter(|&rate| params.test_rate(rate).is_ok())
            .collect();
        if accepted.is_empty() {
            continue;
        }
        sample_rates.extend(accepted.iter().copied());
        formats.push(DeviceFormat {
            label: format.label().to_string(),
            // What the format carries, not what it occupies: `S24_LE`
            // spends four bytes on twenty-four bits of audio.
            bits: match format {
                AlsaSampleFormat::F32 | AlsaSampleFormat::S32 => 32,
                AlsaSampleFormat::S24Packed | AlsaSampleFormat::S24In32 => 24,
                AlsaSampleFormat::S16 => 16,
            },
            float: matches!(format, AlsaSampleFormat::F32),
            sample_rates: accepted,
        });
    }

    Ok(DeviceCapabilities {
        device_id: requested,
        source: CapabilitySource::AlsaHardware,
        formats,
        sample_rates: sample_rates.into_iter().collect(),
        max_channels: hwp.get_channels_max().unwrap_or(0) as u16,
        // The **smallest** period the hardware will take, which is the
        // one quantity every backend can answer — see
        // `DeviceCapabilities::min_period_frames`. Not the period we ask
        // for when playing (`TARGET_PERIOD_FRAMES`): that one is ours,
        // and this sheet describes the device.
        min_period_frames: hwp.get_period_size_min().ok().map(|frames| frames as u32),
        unavailable_reason: None,
    })
}
