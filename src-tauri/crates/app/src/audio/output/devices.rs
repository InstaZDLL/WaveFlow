//! Enumerating the output devices the Settings picker offers: cpal's
//! list, and on Linux the ALSA hints that name each card.

use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

use crate::error::{AppError, AppResult};

#[cfg(target_os = "linux")]
use super::silence_alsa_stderr;

/// Description of one available output device, returned to the
/// frontend by [`list_output_devices`]. The `id` field is the cpal
/// device name — there is no stable platform-independent ID, so we
/// match by name when the user picks one.
#[derive(Debug, Clone, Serialize)]
pub struct OutputDeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// `DeviceTrait::name()` was deprecated in cpal 0.17 in favour of
/// `description()` + `id()`. On Windows, `description().name()` maps
/// to `DEVPKEY_Device_DeviceDesc`, which is the *generic* class name
/// (e.g. literally `"Speakers"`) shared by every endpoint of the same
/// kind — so a system with three speaker-class outputs (onboard DAC,
/// USB headset, Steam virtual sink) shows three indistinguishable
/// `"Speakers"` rows. The disambiguated `DEVPKEY_Device_FriendlyName`
/// (`"Speakers (Logitech PRO X Wireless Gaming Headset)"`) is
/// surfaced by cpal in `description().extended()[0]` when it differs
/// from DeviceDesc — we prefer it for display *and* for the value
/// persisted in `profile_setting['audio.output_device']`, so device
/// selection survives a restart even when several outputs share a
/// class name.
pub(super) fn device_display_name(device: &cpal::Device) -> Option<String> {
    let desc = device.description().ok()?;
    if let Some(friendly) = desc.extended().first() {
        return Some(friendly.clone());
    }
    Some(desc.name().to_string())
}

/// The name a device is known by everywhere the app compares devices: the
/// persisted pin, the listing's `id`, the OS default and the device that
/// actually opened.
///
/// On Linux that is the ALSA PCM id (`pipewire`, `hw:CARD=PCH,DEV=0`),
/// which cpal 0.17 carries as the description's `driver`. The menu there
/// is built from ALSA's hint database and keys its rows on that id, so a
/// pin is an id — while [`device_display_name`] answers with the hint's
/// description ("PipeWire Sound Server"). Comparing the two meant a
/// pinned device was never found when the output opened, fell back to
/// the default every time, and the menu reported the pin unavailable;
/// the OS default was never marked either. Elsewhere the listing keys on
/// the display name, so that is the key.
pub(super) fn device_key(device: &cpal::Device) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let desc = device.description().ok()?;
        if let Some(id) = desc.driver() {
            return Some(id.to_string());
        }
    }
    device_display_name(device)
}

/// Enumerate every output device available on the default audio host.
/// The OS default is flagged so the UI can highlight it.
///
/// On Linux we deliberately **avoid** `cpal::HostTrait::output_devices()`
/// here: cpal's ALSA backend calls `snd_pcm_open()` on every card to
/// build its iterator, which probes hardware (HDMI sinks, Bluetooth
/// profiles, …) and can take 1-2 seconds plus spam stderr with
/// `pcm_dmix` / `pcm_route` warnings. That's the source of the
/// 2-second freeze the user sees when opening the device menu —
/// webkit2gtk renders a black frame while waiting on the IPC. Instead
/// we read the ALSA hint database (`snd_device_name_hint("pcm")`),
/// which is just a config parse and finishes in a few ms with no
/// probing. cpal stays in charge of actually opening the device when
/// the user picks one — we just don't need it for the listing step.
pub fn list_output_devices() -> AppResult<Vec<OutputDeviceInfo>> {
    #[cfg(target_os = "linux")]
    {
        list_output_devices_alsa_hints()
    }
    #[cfg(not(target_os = "linux"))]
    {
        list_output_devices_cpal()
    }
}

/// Fallback enumeration via cpal — used on non-Linux platforms where
/// the host's enumeration is fast enough not to need a workaround.
#[cfg(not(target_os = "linux"))]
fn list_output_devices_cpal() -> AppResult<Vec<OutputDeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|d| device_display_name(&d));
    let devices = host
        .output_devices()
        .map_err(|e| AppError::Audio(format!("enumerate output devices: {e}")))?;
    let mut out = Vec::new();
    for device in devices {
        let Some(name) = device_display_name(&device) else {
            continue;
        };
        let is_default = default_name.as_deref().is_some_and(|n| n == name);
        out.push(OutputDeviceInfo {
            id: name.clone(),
            name,
            is_default,
        });
    }
    Ok(out)
}

/// ALSA name families that are routing or conversion layers rather than
/// something a person would choose (#594).
///
/// Two reasons to hide them, and the second is ours specifically. They
/// make one sound card appear several times under names that mean
/// nothing — `surround40:CARD=PCH`, `dmix:CARD=PCH,DEV=0` — and several
/// of them go through the mixer, so picking one **silently defeats the
/// exclusive output the user has just turned on**: `plughw` is a
/// conversion plug, `dmix` and `dsnoop` are the software mixer itself.
///
/// What is deliberately **not** here: `default`, `pulse` and `pipewire`,
/// which are not hardware but are the right answer for most people most
/// of the time, and on a PipeWire desktop are often the only thing that
/// works; and `hdmi` / `iec958`, which are how those outputs are
/// reached at all.
#[cfg(any(target_os = "linux", test))]
const ALSA_VIRTUAL_FAMILIES: &[&str] = &[
    "plughw",
    "dmix",
    "dsnoop",
    "usbstream",
    "null",
    "speex",
    "speexrate",
    "upmix",
    "vdownmix",
    "samplerate",
    "lavrate",
    "oss",
    "jack",
    "surround21",
    "surround40",
    "surround41",
    "surround50",
    "surround51",
    "surround71",
];

/// One row of the ALSA hint database as we present it: the name a pick
/// sends back to the engine, and what the list shows.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AlsaHintRow {
    pub(super) id: String,
    pub(super) display: String,
}

/// The endpoint an ALSA hint name denotes: its family, and the card and
/// device it reaches when it names one.
///
/// `hw:CARD=PCH,DEV=0` and `front:CARD=PCH,DEV=0` are the same hardware
/// through two families, which is what makes the card token — the
/// closest thing a hint carries to a driver identity — the key to
/// compare on rather than the name.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AlsaEndpoint<'a> {
    family: &'a str,
    card: Option<&'a str>,
    /// Defaulted to `0` when the name leaves it out (`sysdefault:CARD=X`
    /// reaches that card's first device), so the two spellings compare
    /// equal.
    dev: &'a str,
}

#[cfg(any(target_os = "linux", test))]
fn parse_alsa_hint_name(name: &str) -> AlsaEndpoint<'_> {
    let (family, args) = match name.split_once(':') {
        Some((family, args)) => (family, args),
        None => (name, ""),
    };
    let mut card = None;
    let mut dev = "0";
    for arg in args.split(',') {
        match arg.split_once('=') {
            Some(("CARD", value)) => card = Some(value),
            Some(("DEV", value)) => dev = value,
            _ => {}
        }
    }
    AlsaEndpoint { family, card, dev }
}

/// Turn the raw hint rows into the list the picker shows (#594).
///
/// Pure, and separate from the enumeration, because the enumeration
/// itself must not change: it reads ALSA's hint database rather than
/// opening every PCM, which is what keeps the device menu from freezing
/// for one to two seconds. Everything below works from what the hints
/// already carry.
///
/// Three passes, in this order:
///
/// 1. drop the routing families ([`ALSA_VIRTUAL_FAMILIES`]);
/// 2. drop a family that only wraps hardware we are already showing —
///    `front:CARD=PCH,DEV=0` next to `hw:CARD=PCH,DEV=0` is the same
///    output twice, and the `hw` spelling is the one exclusive output
///    can actually use. Matching on the card token rather than on the
///    name is what makes that work across families;
/// 3. disambiguate rows that would read identically. Two identical
///    cards produce the same description, and presenting them as one
///    entry loses the second device entirely — the id differs, so the
///    pick works, but nothing on screen said there were two.
#[cfg(any(target_os = "linux", test))]
pub(super) fn present_alsa_hints(rows: Vec<AlsaHintRow>) -> Vec<AlsaHintRow> {
    use std::collections::{HashMap, HashSet};

    let mut kept: Vec<AlsaHintRow> = rows
        .into_iter()
        .filter(|row| {
            let endpoint = parse_alsa_hint_name(&row.id);
            !ALSA_VIRTUAL_FAMILIES.contains(&endpoint.family)
        })
        .collect();

    // Owned, so the `retain` below can borrow each row while reading it.
    let hardware: HashSet<(String, String)> = kept
        .iter()
        .filter_map(|row| {
            let endpoint = parse_alsa_hint_name(&row.id);
            match (endpoint.family, endpoint.card) {
                ("hw", Some(card)) => Some((card.to_string(), endpoint.dev.to_string())),
                _ => None,
            }
        })
        .collect();
    kept.retain(|row| {
        let endpoint = parse_alsa_hint_name(&row.id);
        endpoint.family == "hw"
            || !endpoint.card.is_some_and(|card| {
                hardware.contains(&(card.to_string(), endpoint.dev.to_string()))
            })
    });

    let mut occurrences: HashMap<&str, usize> = HashMap::new();
    for row in &kept {
        *occurrences.entry(row.display.as_str()).or_default() += 1;
    }
    let ambiguous: HashSet<String> = occurrences
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(display, _)| display.to_string())
        .collect();
    // How many of the colliding rows each card token accounts for: a
    // token shared by two of them tells them apart no better than the
    // description did.
    let mut by_card: HashMap<(&str, &str), usize> = HashMap::new();
    for row in &kept {
        if ambiguous.contains(&row.display) {
            let endpoint = parse_alsa_hint_name(&row.id);
            if let Some(card) = endpoint.card {
                *by_card.entry((row.display.as_str(), card)).or_default() += 1;
            }
        }
    }
    let suffixes: Vec<Option<String>> = kept
        .iter()
        .map(|row| {
            if !ambiguous.contains(&row.display) {
                return None;
            }
            let endpoint = parse_alsa_hint_name(&row.id);
            // The card token when it is the thing that differs — two
            // cards of the same model — and the raw name otherwise. A
            // card with two devices under one description
            // (`hw:CARD=PCH,DEV=0` and `,DEV=2`) would be given the same
            // token twice, which is the duplicate all over again; the id
            // is unique by construction.
            match endpoint
                .card
                .filter(|card| by_card.get(&(row.display.as_str(), *card)) == Some(&1))
            {
                Some(card) => Some(card.to_string()),
                None => Some(row.id.clone()),
            }
        })
        .collect();
    for (row, suffix) in kept.iter_mut().zip(suffixes) {
        if let Some(suffix) = suffix {
            row.display = format!("{} ({suffix})", row.display);
        }
    }
    kept
}

/// Linux-only fast enumeration via ALSA's hint API. Same data as
/// `aplay -L` exposes — config-level info, no PCM probing — so it
/// returns instantly even on systems with many HDMI cards.
///
/// We still need cpal for the *default device's name* so we can flag
/// the right row, but `default_output_device()` is a single-device
/// lookup that doesn't iterate.
#[cfg(target_os = "linux")]
fn list_output_devices_alsa_hints() -> AppResult<Vec<OutputDeviceInfo>> {
    use std::collections::HashSet;
    use std::ffi::CString;

    // cpal's `default_output_device` opens just the "default" alias
    // (one open, fast) and reports its resolved name. We use that to
    // mark which hint row should carry `is_default = true`. Wrapped
    // in `silence_alsa_stderr` to swallow any tangential probe noise.
    let default_name = silence_alsa_stderr(|| {
        cpal::default_host()
            .default_output_device()
            .and_then(|d| device_key(&d))
    });

    let pcm = CString::new("pcm").map_err(|e| AppError::Audio(format!("CString: {e}")))?;
    let iter = alsa::device_name::HintIter::new(None, pcm.as_c_str())
        .map_err(|e| AppError::Audio(format!("ALSA HintIter: {e}")))?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut rows = Vec::new();
    for hint in iter {
        // Filter to playback-capable devices. ALSA hints with no
        // `direction` field can be either, so we keep them.
        let direction_ok = matches!(hint.direction, None | Some(alsa::Direction::Playback));
        if !direction_ok {
            continue;
        }
        let Some(name) = hint.name else { continue };
        // ALSA reports the same hint multiple times in some configs
        // (once per profile). Dedupe by name.
        if !seen.insert(name.clone()) {
            continue;
        }
        let display = hint
            .desc
            .map(|d| d.replace('\n', ", "))
            .unwrap_or_else(|| name.clone());
        rows.push(AlsaHintRow { id: name, display });
    }

    // What to actually show (#594): the aliases and routing layers are
    // dropped here rather than during the walk above, so the rule is one
    // pure function the tests can exercise without a sound card.
    Ok(present_alsa_hints(rows)
        .into_iter()
        .map(|row| {
            let is_default = default_name.as_deref().is_some_and(|d| d == row.id);
            OutputDeviceInfo {
                id: row.id,
                name: row.display,
                is_default,
            }
        })
        .collect())
}
