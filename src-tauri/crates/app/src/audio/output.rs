//! cpal output stream, hosted on a dedicated thread.
//!
//! On Windows `cpal::Stream` is `!Send` (WASAPI / COM handles don't
//! cross thread boundaries). To keep the Stream alive for the engine's
//! lifetime without forcing it into Tauri's managed state (which demands
//! `Send + Sync`), we spawn an "output thread" that:
//!
//! 1. creates the cpal device + stream locally (so the `!Send` value
//!    never leaves its origin thread),
//! 2. calls `stream.play()`,
//! 3. parks on a shutdown channel until the engine tears down.
//!
//! The decoder-side `Producer<f32>` is `Send` and is handed back to the
//! caller along with the shutdown sender and the thread's join handle.
//!
//! The audio callback itself MUST NOT take locks, allocate, or block —
//! it only reads from `rtrb::Consumer` and mutates atomics in
//! [`SharedPlayback`].

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::error::{AppError, AppResult};

use super::state::{PlayerState, SharedPlayback};

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
fn device_display_name(device: &cpal::Device) -> Option<String> {
    let desc = device.description().ok()?;
    if let Some(friendly) = desc.extended().first() {
        return Some(friendly.clone());
    }
    Some(desc.name().to_string())
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
struct AlsaHintRow {
    id: String,
    display: String,
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
fn present_alsa_hints(rows: Vec<AlsaHintRow>) -> Vec<AlsaHintRow> {
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
            .and_then(|d| device_display_name(&d))
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

/// Run the closure while ALSA library error messages are redirected
/// to /dev/null. On Linux, cpal's enumeration probes every PCM card
/// and ALSA helpfully prints `pcm_dmix` / `pcm_route` warnings for
/// cards that aren't currently usable (HDMI sinks with no monitor
/// attached, Bluetooth profiles in the wrong state, …). The warnings
/// are noise — failure to open during a probe is expected — so we
/// hide them while we're enumerating.
///
/// On non-Linux platforms this is a passthrough.
#[cfg(target_os = "linux")]
fn silence_alsa_stderr<R, F: FnOnce() -> R>(f: F) -> R {
    use std::os::unix::io::AsRawFd;

    // Open /dev/null + dup the current stderr (fd 2) so we can put it
    // back. If anything fails, just run `f` with stderr untouched —
    // we'd rather show the spam than skip enumeration.
    let dev_null = match std::fs::OpenOptions::new().write(true).open("/dev/null") {
        Ok(f) => f,
        Err(_) => return f(),
    };
    let saved = unsafe { libc::dup(2) };
    if saved < 0 {
        return f();
    }
    if unsafe { libc::dup2(dev_null.as_raw_fd(), 2) } < 0 {
        unsafe { libc::close(saved) };
        return f();
    }
    let result = f();
    // Restore stderr — best-effort. If `dup2` fails here we can't do
    // much, but the OS will reclaim the fd at process exit.
    unsafe {
        libc::dup2(saved, 2);
        libc::close(saved);
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn silence_alsa_stderr<R, F: FnOnce() -> R>(f: F) -> R {
    f()
}

/// Capacity of the SPSC sample ring, in f32 samples. At 48 kHz stereo
/// this is ~1 second of audio, which gives the decoder thread plenty of
/// headroom while keeping latency low.
pub const RING_CAPACITY: usize = 96_000;

/// Exact (rate, channels) an output must open at to carry a DoP (DSD
/// over PCM) stream, #495. Unlike the normal path — where the device
/// picks the rate and the decoder resamples to it — a DoP stream must
/// reach the DAC at precisely `dsd_rate / 16` in 24-bit or the marker
/// cadence breaks. So the caller (the engine, when it loads a DSD track
/// with DoP enabled) hands the exclusive backend this forced format
/// instead of letting it negotiate. Ignored entirely on the cpal shared
/// path — DoP only runs over an exclusive backend: WASAPI Exclusive on
/// Windows, a raw ALSA `hw:` device on Linux, CoreAudio hog mode on
/// macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DopFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// A format the caller demands of the output for one track.
///
/// Two demands ride this, and they differ in what a refusal means:
///
/// - **DoP** (#495) pins the rate *and* the channel layout, and must not
///   fall back to another rate — a marker cadence that gets resampled is
///   white noise — so a refusal fails the open outright and the caller
///   plays the track as DSD → PCM instead.
/// - **The track's own rate** (#600) pins the rate only, and is a
///   preference: a device that will not open at it falls back to the
///   rates it does offer, and the decoder's resampler meets it exactly as
///   it always did. The channel layout stays the device's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestedFormat {
    pub sample_rate: u32,
    /// Part of the format for DoP; `None` for a rate request.
    pub channels: Option<u16>,
    /// Whether a refusal is fatal — see above.
    pub dop: bool,
}

impl RequestedFormat {
    /// A DoP demand: both axes pinned, no fallback.
    pub fn dop(format: DopFormat) -> Self {
        Self {
            sample_rate: format.sample_rate,
            channels: Some(format.channels),
            dop: true,
        }
    }

    /// A request to open at this track's own rate (#600).
    pub fn source_rate(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            channels: None,
            dop: false,
        }
    }

    /// The DoP format this request carries, for the handle's record and
    /// for the backends that branch on it. `None` for a rate request,
    /// which is not DoP however exact it turns out to be.
    pub fn as_dop(self) -> Option<DopFormat> {
        self.dop.then_some(DopFormat {
            sample_rate: self.sample_rate,
            // A DoP request always carries its channel count; two is the
            // only sane reading of a malformed one.
            channels: self.channels.unwrap_or(2),
        })
    }
}

/// Pick the right output backend based on the runtime preference.
/// With `exclusive=true`, tries the platform's exclusive backend first
/// and falls back to cpal shared if init fails (device busy, no
/// supported format, COM apartment conflict, …): WASAPI Exclusive on
/// Windows, a raw `hw:` device on Linux, hog mode on macOS. With
/// `exclusive=false`, always cpal.
///
/// The fallback is silent at the caller level — the warning is logged
/// so the user can see in `waveflow.log` why exclusive didn't engage.
pub fn spawn_output_with_mode(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    device_name: Option<String>,
    exclusive: bool,
    requested: Option<RequestedFormat>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    // DoP (#495) is a hard requirement, not a preference: a DoP stream
    // that can't open its exact rate in exclusive form must NOT fall back
    // to a shared / mixed output (the OS mixer would resample the marker
    // cadence into white noise) — the caller instead falls back to
    // ordinary DSD → PCM. So when a DoP format is requested we try the
    // platform's exclusive backend only, and surface its error verbatim:
    // WASAPI Exclusive on Windows, a raw `hw:` device on Linux (ALSA),
    // hog mode on macOS (CoreAudio).
    //
    // A *rate* request (#600) is the other kind and deliberately does not
    // take this branch: it is a preference, so it goes down the ordinary
    // path below and keeps the shared-mode fallback that path provides.
    if let Some(dop) = requested.filter(|r| r.dop) {
        #[cfg(target_os = "windows")]
        return super::wasapi_exclusive::spawn_exclusive_output_thread(
            shared,
            app,
            device_name,
            Some(dop),
        );
        #[cfg(target_os = "linux")]
        return super::alsa_exclusive::spawn_alsa_exclusive_output_thread(
            shared,
            app,
            device_name,
            Some(dop),
        );
        #[cfg(target_os = "macos")]
        return super::coreaudio_exclusive::spawn_coreaudio_exclusive_output_thread(
            shared,
            app,
            device_name,
            Some(dop),
        );
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        {
            let _ = (dop, &shared, &app, &device_name);
            return Err(AppError::Audio(
                "DoP output is not supported on this platform".into(),
            ));
        }
    }

    #[cfg(target_os = "windows")]
    if exclusive {
        match super::wasapi_exclusive::spawn_exclusive_output_thread(
            shared.clone(),
            app.clone(),
            device_name.clone(),
            requested,
        ) {
            Ok(pair) => {
                tracing::info!("audio output: WASAPI Exclusive Mode engaged");
                return Ok(pair);
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    "WASAPI Exclusive init failed, falling back to shared mode"
                );
            }
        }
    }
    // Linux: the same bargain through a raw `hw:` device. The card is
    // usually held by PipeWire or PulseAudio at this point, so the
    // backend asks for it through the reservation protocol before
    // concluding it can't be had.
    #[cfg(target_os = "linux")]
    if exclusive {
        match super::alsa_exclusive::spawn_alsa_exclusive_output_thread(
            shared.clone(),
            app.clone(),
            device_name.clone(),
            requested,
        ) {
            Ok(pair) => {
                tracing::info!("audio output: ALSA exclusive (raw hw:) engaged");
                return Ok(pair);
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    "ALSA exclusive init failed, falling back to shared mode"
                );
            }
        }
    }

    // macOS: hog mode, which stops the system mixing anything else into
    // the device. Unlike the DoP path it leaves the device's physical
    // format alone — re-clocking a device the whole machine shares is a
    // price only a marker cadence justifies paying.
    #[cfg(target_os = "macos")]
    if exclusive {
        match super::coreaudio_exclusive::spawn_coreaudio_exclusive_output_thread(
            shared.clone(),
            app.clone(),
            device_name.clone(),
            requested,
        ) {
            Ok(pair) => {
                tracing::info!("audio output: CoreAudio exclusive (hog mode) engaged");
                return Ok(pair);
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    "CoreAudio exclusive init failed, falling back to shared mode"
                );
            }
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    let _ = (exclusive, requested); // no exclusive PCM backend on this target

    spawn_output_thread(shared, app, device_name)
}

/// Surface a lost audio device to the user: park the player, tell the
/// UI, and keep the OS media controls in sync.
///
/// Shared by the cpal error callback and the WASAPI-exclusive event
/// loop ([`super::wasapi_exclusive`]) so both backends report a device
/// loss identically — the exclusive thread used to just exit, leaving
/// the UI convinced playback was still running.
pub(super) fn notify_device_lost(app: &AppHandle, shared: &Arc<SharedPlayback>, message: String) {
    // Stamped here, at the error itself, rather than in the recovery that
    // follows 300 ms later: the same unplug also moves the system default,
    // and the follow that reacts to that (#627) has to know this is a
    // device going away, not a preference changing (#617). The engine can
    // be missing while `AudioEngine::new` is still running, which is the
    // one window where no recovery is scheduled either.
    if let Some(engine) = app.try_state::<std::sync::Arc<super::AudioEngine>>() {
        engine.note_device_loss();
    }
    shared.set_state(PlayerState::Paused);
    let _ = app.emit(
        "player:state",
        json!({ "state": "paused", "track_id": null }),
    );
    // The message is technical on purpose — it is what a bug report
    // needs — and `kind` is what the UI turns into a sentence the user
    // can act on (#597).
    let _ = app.emit(
        "player:error",
        json!({ "message": message, "kind": "device-lost" }),
    );
    if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
        controls.update_playback(PlayerState::Paused, shared.current_position_ms());
    }
}

/// Which device a scheduled rebuild should reopen.
///
/// The two callers differ in whether the engine can still resolve the
/// device from its live handle at rebuild time:
/// - the cpal error callback and the WASAPI-exclusive `DeviceLost` exit
///   both fire while the (now-dead) handle is still parked in
///   `self.output`, so its `device_name` is still readable → [`Resolve`];
/// - the `set_exclusive_output` failure path has already `take()`n the
///   old handle, emptying `self.output`, so a self-resolve would return
///   `None` and reopen the OS default instead of the user's pick (#405)
///   → [`Device`] carries the device captured before the teardown.
///
/// [`Resolve`]: RebuildTarget::Resolve
/// [`Device`]: RebuildTarget::Device
pub(super) enum RebuildTarget {
    /// Resolve the device from the engine's live handle at rebuild time.
    Resolve,
    /// Reopen this specific device (`None` = OS default).
    Device(Option<String>),
}

/// Schedule a same-device rebuild of the output stream after a device
/// loss (#175). Returns immediately — the work happens on a tokio task.
///
/// 300 ms backoff first (covers the OS settling a USB replug / driver
/// restart / Windows session reset), then a SAME-DEVICE rebuild through
/// the engine. Re-querying the OS default here would land the user on a
/// different output every time their pinned device flapped.
///
/// The engine is resolved AFTER the delay on purpose — an error can fire
/// while `AudioEngine::new` is still running, i.e. before the engine has
/// been registered in Tauri's managed state, and looking it up eagerly
/// would silently drop the recovery for that window.
pub(super) fn schedule_device_rebuild(app: &AppHandle, target: RebuildTarget) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let Some(engine) = app.try_state::<std::sync::Arc<super::AudioEngine>>() else {
            return;
        };
        // #365: one rebuild per burst of device errors. A DAC that
        // resets on every exclusive grab makes our own re-open
        // trigger the next error, so without a gate the passes
        // chase each other until one opens exclusive while the
        // previous stream is still settling and collides with
        // AUDCLNT_E_DEVICE_IN_USE. Gating here (rather than before
        // the sleep) still collapses the burst: whichever task
        // wakes first arms, and the others hit either `armed` or
        // the settle window.
        if !engine.try_arm_device_rebuild() {
            tracing::debug!(
                "device rebuild already armed or within the settle window; \
                 ignoring this device loss"
            );
            return;
        }
        // The rebuild is synchronous and genuinely slow: it joins
        // the old output thread — which, in WASAPI exclusive mode,
        // can sit in `wait_for_event` up to its 2 s timeout — and
        // then opens the device inline (COM init + the #174 format
        // negotiation ladder). Running that directly on a tokio
        // worker would park the worker for the whole duration, so
        // hand it to the blocking pool.
        let engine = engine.inner().clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(err) = engine.try_rebuild_after_device_error(target) {
                tracing::warn!(%err, "auto-rebuild after device loss failed");
            }
        })
        .await;
    });
}

/// Name of the OS default output device, as the pickers and the open
/// paths see it.
///
/// Same accessor the device listing flags its default row with, so a
/// name from here is comparable with `OutputHandle::opened_device`
/// (#627) — the shared cpal path fills that field from
/// [`device_display_name`] too. `None` means the host reports no
/// default output at all.
pub(super) fn os_default_output_name() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        // Wrapped for the same reason the listing wraps it: opening the
        // `default` alias can make ALSA chatter on stderr.
        silence_alsa_stderr(|| {
            cpal::default_host()
                .default_output_device()
                .and_then(|d| device_display_name(&d))
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        cpal::default_host()
            .default_output_device()
            .and_then(|d| device_display_name(&d))
    }
}

/// How many times a deferred default-device follow comes back before
/// giving up. Three attempts spread over ~6 s outlast one rebuild and
/// its settle window; past that, a gate this busy means device errors
/// are storming, and the recovery — which reopens the OS default when
/// nothing is pinned — is the better judge of where to land.
const FOLLOW_DEFAULT_ATTEMPTS: usize = 3;

/// Added to the settle window before a retry, so the retry wakes just
/// *after* it rather than racing its boundary.
const FOLLOW_RETRY_MARGIN: std::time::Duration = std::time::Duration::from_millis(100);

/// Schedule a move onto the system's new default output (#627).
/// Returns immediately — the work happens on a tokio task.
///
/// Called from the platform listeners in
/// [`super::default_device`], which run on an OS thread and must not
/// block. Everything that decides *whether* to move lives in
/// [`super::AudioEngine::follow_os_default_output`]; this function is
/// only the hop onto our own runtime, plus the two delays that make the
/// storm survivable:
///
/// - the same 300 ms backoff the device-loss recovery takes, because a
///   default change arrives while the OS is still settling — Windows
///   fires the notification per role, and a freshly connected endpoint
///   is not necessarily openable the instant it becomes the default;
/// - the #365 rebuild gate, which collapses the burst to one rebuild
///   and then holds a settle window over the device error our own
///   reopen provokes on the outgoing stream.
///
/// A gate that is busy defers rather than drops: the follow is retried
/// after the settle window, a bounded number of times. Without that, a
/// default moved twice inside two seconds — or moved while the recovery
/// was rebuilding — would leave the stream on the intermediate device
/// for good, because each change is one notification and not a
/// repeating signal. Each retry re-reads the pin and the default, so it
/// converges on the current state rather than replaying a stale one.
pub(super) fn schedule_default_device_follow(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let Some(engine) = app.try_state::<std::sync::Arc<super::AudioEngine>>() else {
            // A notification during boot, before the engine reached
            // Tauri state. Nothing to move: the open that follows will
            // read the new default anyway.
            return;
        };
        // The #365 gate is NOT armed here. `follow_os_default_output`
        // arms it itself, once its cheap checks say a rebuild is really
        // going to happen: arming for a follow that turns out to be a
        // no-op — a pinned device, a default that is already the endpoint
        // we are on — would open a settle window for nothing, and a real
        // `DeviceNotAvailable` landing inside it is dropped and never
        // retried, leaving the user with no output at all.
        //
        // Synchronous and genuinely slow — it joins the old output
        // thread and opens the device inline — so it goes to the
        // blocking pool rather than parking a tokio worker.
        let engine = engine.inner().clone();
        for attempt in 1..=FOLLOW_DEFAULT_ATTEMPTS {
            let engine = engine.clone();
            let joined =
                tokio::task::spawn_blocking(move || engine.follow_os_default_output()).await;
            let outcome = match joined {
                Ok(outcome) => outcome,
                Err(err) => {
                    // The blocking task panicked or was cancelled. Say so
                    // rather than reporting a follow that never ran as a
                    // settled one, and stop: a retry would most likely
                    // panic in the same place.
                    tracing::warn!(%err, "default-device follow task did not finish");
                    return;
                }
            };
            match outcome {
                Ok(super::engine::FollowOutcome::Settled) => return,
                Ok(super::engine::FollowOutcome::Deferred) => {
                    if attempt == FOLLOW_DEFAULT_ATTEMPTS {
                        tracing::warn!(
                            attempts = FOLLOW_DEFAULT_ATTEMPTS,
                            "gave up following the new default output device: the rebuild \
                             gate stayed busy"
                        );
                        return;
                    }
                    // Wake just past the settle window the busy rebuild
                    // opened, so the retry finds the gate free.
                    tokio::time::sleep(super::engine::REBUILD_SETTLE_WINDOW + FOLLOW_RETRY_MARGIN)
                        .await;
                }
                Err(err) => {
                    tracing::warn!(%err, "following the new default output device failed");
                    return;
                }
            }
        }
    });
}

/// Drain one period out of the ring into `samples`, applying the same
/// per-sample chain the cpal callback applies: volume, the normalize
/// attenuation, and the optional mono downmix. Shared by all three
/// exclusive backends.
///
/// Returns how many samples were actually pulled. Silence written
/// because the ring ran dry is deliberately NOT counted — every backend
/// agrees on that, because `samples_played` is the only clock the
/// progress bar, the lyrics sync and play-event crediting have, and
/// crediting an underrun would make the track run ahead of itself.
pub(super) fn fill_pcm_period(
    shared: &SharedPlayback,
    consumer: &mut Consumer<f32>,
    samples: &mut [f32],
    channels: usize,
) -> u64 {
    let volume = shared.volume();
    let normalize = shared.normalize_enabled.load(Ordering::Relaxed);
    let mono = shared.mono_enabled.load(Ordering::Relaxed);
    // Normalization applies a -3 dB reduction to leave headroom.
    let norm_gain: f32 = if normalize { 0.707 } else { 1.0 };
    let mut written: u64 = 0;

    if mono && channels >= 2 {
        for frame in samples.chunks_mut(channels) {
            let mut sum = 0.0_f32;
            let mut got = 0usize;
            for _ in 0..frame.len() {
                if let Ok(s) = consumer.pop() {
                    sum += s;
                    got += 1;
                }
            }
            let value = if got > 0 {
                written += got as u64;
                (sum / channels as f32) * volume * norm_gain
            } else {
                0.0
            };
            for slot in frame.iter_mut() {
                *slot = value;
            }
        }
    } else {
        for slot in samples.iter_mut() {
            *slot = match consumer.pop() {
                Ok(s) => {
                    written += 1;
                    s * volume * norm_gain
                }
                Err(_) => 0.0,
            };
        }
    }

    written
}

/// Handle retained by the engine so it can tear the output thread down
/// cleanly on shutdown or device switch. Separate from the decoder-side
/// `Producer` which is handed off independently — see the tuple returned
/// from [`spawn_output_thread`].
pub struct OutputHandle {
    pub shutdown_tx: Sender<()>,
    pub join: JoinHandle<()>,
    /// Device name this output was *asked* for — `None` means the OS
    /// default. It stays the request even when the backend fell back to
    /// the default because the name was no longer enumerated: the picker
    /// highlights it, a same-device pick no-ops on it, and a rebuild or a
    /// DoP reopen goes back to it, so the user's pin survives until the
    /// device returns. That also makes it no endpoint identity.
    pub device_name: Option<String>,
    /// Device this output actually opened, when the backend could name
    /// it. Distinct from [`Self::device_name`] on purpose: that one is
    /// the user's pin and must survive a fallback, this one is where
    /// the audio really goes. They differ whenever the pinned name was
    /// no longer enumerated and the backend silently took the default
    /// instead — the case where the picker used to tick a device that
    /// was not playing anything (#612). `None` means the backend opened
    /// something it cannot name, which is reported as "unknown" rather
    /// than guessed.
    pub opened_device: Option<String>,
    /// Whether this handle really owns its device — WASAPI Exclusive
    /// Mode on Windows, a raw `hw:` handle on Linux. The user
    /// preference can request it, but startup may fall back to cpal
    /// shared mode when the device rejects it.
    pub exclusive: bool,
    /// `Some(fmt)` when this output is carrying a DoP (DSD over PCM)
    /// stream, opened at exactly that rate (`dsd_rate / 16`) and channel
    /// count, #495. `None` for every ordinary PCM output. The engine
    /// reads it to decide whether the next track needs an output
    /// rebuild: a DoP track whose format differs in *any* field, or any
    /// PCM track after a DoP one, forces a re-open; a DoP track with the
    /// identical format can reuse it. The channel count is part of the
    /// comparison because the exclusive stream is opened for a fixed
    /// interleave — a stereo output handed 5.1 DoP frames would tear
    /// them across channels.
    pub dop: Option<DopFormat>,
}

impl OutputHandle {
    /// Signal the output thread to drop its Stream and exit, then wait
    /// for it. Called from `AudioEngine::shutdown` and `Drop`.
    pub fn stop(self) {
        // Ignore the send error — the receiver may already be gone if
        // the stream errored out on its own.
        let _ = self.shutdown_tx.send(());
        let _ = self.join.join();
    }
}

/// Spawn the dedicated output thread. Returns the decoder-side
/// `Producer<f32>` (hand it to the decoder) and an [`OutputHandle`] the
/// engine keeps around for teardown.
///
/// The thread is named `waveflow-audio-output` so it's easy to spot in
/// profilers / `perf top`. Any error during Stream construction is
/// surfaced via an init-result channel before this function returns, so
/// the caller learns synchronously whether playback is usable.
///
/// Takes an [`AppHandle`] so the cpal error callback can emit
/// `player:error` + `player:state` events on device loss (headphones
/// unplugged mid-playback).
pub fn spawn_output_thread(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    device_name: Option<String>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<Option<String>>>(1);

    let thread_shared = shared.clone();
    let thread_app = app.clone();
    let thread_device = device_name.clone();
    let join = std::thread::Builder::new()
        .name("waveflow-audio-output".into())
        .spawn(move || {
            output_thread_main(
                thread_shared,
                consumer,
                shutdown_rx,
                init_tx,
                thread_app,
                thread_device,
            )
        })
        .map_err(|e| AppError::Audio(format!("spawn output thread: {e}")))?;

    // Block until the thread reports whether the Stream opened cleanly.
    // Any failure here means we never reached `stream.play()`.
    match init_rx.recv() {
        Ok(Ok(opened_device)) => Ok((
            producer,
            OutputHandle {
                shutdown_tx,
                join,
                device_name,
                opened_device,
                exclusive: false,
                dop: None,
            },
        )),
        Ok(Err(err)) => {
            // The thread already exited; join it so we don't leak.
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(AppError::Audio(
            "output thread died before reporting init result".into(),
        )),
    }
}

/// Thread body. Owns the `!Send` `cpal::Stream` locally, so nothing
/// crosses a thread boundary.
fn output_thread_main(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    init_tx: Sender<AppResult<Option<String>>>,
    app: AppHandle,
    device_name: Option<String>,
) {
    let (stream, opened_device) =
        match build_stream(shared.clone(), consumer, app.clone(), device_name) {
            Ok(pair) => pair,
            Err(err) => {
                let _ = init_tx.send(Err(err));
                return;
            }
        };

    if let Err(err) = stream
        .play()
        .map_err(|e| AppError::Audio(format!("stream play: {e}")))
    {
        let _ = init_tx.send(Err(err));
        return;
    }

    // Signal successful initialization, naming the endpoint that really
    // opened so the engine can report it rather than the request.
    let _ = init_tx.send(Ok(opened_device));

    // Park until the engine says shutdown. The Stream runs its callback
    // on its own (WASAPI-managed) thread on Windows, so we just need to
    // keep the Stream alive here.
    let _ = shutdown_rx.recv();
    drop(stream);
    tracing::debug!("audio output thread exiting");
}

/// Build the cpal `Stream`. Called only from inside the output thread.
///
/// Returns the device actually opened alongside the stream: the lookup
/// below falls back to the default endpoint when the pinned name is no
/// longer enumerated, and the caller has no other way to learn that
/// (#612).
fn build_stream(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    app: AppHandle,
    device_name: Option<String>,
) -> AppResult<(Stream, Option<String>)> {
    silence_alsa_stderr(|| build_stream_inner(shared, consumer, app, device_name))
}

fn build_stream_inner(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    app: AppHandle,
    device_name: Option<String>,
) -> AppResult<(Stream, Option<String>)> {
    let host = cpal::default_host();
    // If a specific device was requested, look it up by name. If the
    // user picked a device that since vanished (USB DAC unplugged
    // between sessions), fall back to the OS default rather than
    // erroring out — the alternative is a silent app on next launch.
    let device = match device_name.as_deref() {
        Some(name) => {
            let mut found = None;
            if let Ok(iter) = host.output_devices() {
                for d in iter {
                    if device_display_name(&d).as_deref() == Some(name) {
                        found = Some(d);
                        break;
                    }
                }
            }
            match found {
                Some(d) => d,
                None => {
                    tracing::warn!(
                        device = %name,
                        "requested output device not found, falling back to default"
                    );
                    host.default_output_device()
                        .ok_or_else(|| AppError::Audio("no default audio output device".into()))?
                }
            }
        }
        None => host
            .default_output_device()
            .ok_or_else(|| AppError::Audio("no default audio output device".into()))?,
    };

    // Read the name off the device we ended up with, not the one we
    // asked for. These differ exactly when the fallback above fired,
    // which is the case the picker used to misreport (#612).
    let opened_device = device_display_name(&device);

    let default_cfg = device
        .default_output_config()
        .map_err(|e| AppError::Audio(format!("default output config: {e}")))?;

    let sample_format = default_cfg.sample_format();
    let channels = default_cfg.channels();
    let sample_rate = default_cfg.sample_rate();
    let config: StreamConfig = default_cfg.into();

    // Stamp the device config into the shared state so the decoder and
    // position helper can compute timing without re-querying cpal.
    shared.sample_rate.store(sample_rate, Ordering::Release);
    shared.channels.store(channels, Ordering::Release);

    tracing::info!(
        sample_rate,
        channels,
        ?sample_format,
        "cpal output stream opened"
    );

    let stream = match sample_format {
        SampleFormat::F32 => open_stream::<f32>(&device, &config, consumer, shared, app),
        SampleFormat::I16 => open_stream::<i16>(&device, &config, consumer, shared, app),
        SampleFormat::U16 => open_stream::<u16>(&device, &config, consumer, shared, app),
        other => Err(AppError::Audio(format!(
            "unsupported sample format: {other:?}"
        ))),
    }?;
    Ok((stream, opened_device))
}

/// Generic stream builder parameterized by the device's native sample
/// format. We always decode into `f32` internally and let cpal convert
/// to whatever the device wants at the last second via `FromSample`.
fn open_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: Consumer<f32>,
    shared: Arc<SharedPlayback>,
    app: AppHandle,
) -> AppResult<Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + Send + 'static,
{
    // On device loss (headphones unplugged, sound server restart)
    // cpal fires this callback on a random thread. We flip state to
    // Paused, emit player:state + player:error so the UI can surface
    // the problem, and keep the Stream itself untouched — it's about
    // to be dropped by cpal anyway.
    //
    // On `DeviceNotAvailable` (#175) we additionally schedule an
    // auto-rebuild — see [`schedule_device_rebuild`]. The engine's
    // debounce guard coalesces a quick double-flap into a single
    // rebuild attempt.
    let err_shared = shared.clone();
    let err_app = app.clone();
    let err_fn = move |err: cpal::StreamError| {
        tracing::warn!(?err, "cpal stream error");
        notify_device_lost(&err_app, &err_shared, format!("audio device error: {err}"));

        if matches!(err, cpal::StreamError::DeviceNotAvailable) {
            // The erroring handle is still parked in the engine's
            // `self.output`, so the rebuild can self-resolve its device.
            schedule_device_rebuild(&err_app, RebuildTarget::Resolve);
        }
    };

    let stream = device
        .build_output_stream(
            config,
            move |out: &mut [T], _info: &cpal::OutputCallbackInfo| {
                // Hard pause: while paused, the decoder stops pushing
                // into the ring, and we stop draining it so the next
                // `resume` picks right back up where it left off. We
                // just write silence into the device buffer — users
                // hear the pause within ~WASAPI's internal latency
                // (~50-200 ms) instead of the full ring length.
                if shared.paused_output.load(Ordering::Acquire) {
                    for slot in out.iter_mut() {
                        *slot = T::from_sample(0.0_f32);
                    }
                    return;
                }

                // Drain-silent mode: drop whatever's left in the
                // ring as fast as possible and write silence. Used
                // during a track switch / seek so the tail of the
                // old position never reaches the device. We pop in
                // bulk (not one-per-output-slot) so the decoder's
                // spin-wait on `producer.slots() == RING_CAPACITY`
                // completes in a single callback (~10-15 ms) instead
                // of waiting for the device to consume the whole ring
                // at its native cadence (~270 ms at 44.1 kHz / 8 ch).
                if shared.drain_silent.load(Ordering::Acquire) {
                    while consumer.pop().is_ok() {}
                    for slot in out.iter_mut() {
                        *slot = T::from_sample(0.0_f32);
                    }
                    return;
                }

                // Read atomic flags once per buffer — cheap relaxed
                // loads that avoid ~5k redundant ops per callback.
                let volume = shared.volume();
                let normalize = shared.normalize_enabled.load(Ordering::Relaxed);
                let mono = shared.mono_enabled.load(Ordering::Relaxed);
                let channels = shared.channels.load(Ordering::Relaxed).max(1) as usize;
                // Normalization applies a −3 dB gain reduction (× 0.707)
                // to prevent clipping on loud source material.
                let norm_gain: f32 = if normalize { 0.707 } else { 1.0 };

                let mut written: u64 = 0;

                if mono && channels >= 2 {
                    // Mono downmix: read `channels` samples at a time,
                    // average them, and write the same value to every
                    // output channel. This loop processes one frame (all
                    // channels) per iteration. If the ring underruns
                    // mid-frame we still write silence for the remaining
                    // channels so the device buffer stays aligned.
                    for frame in out.chunks_mut(channels) {
                        let mut sum: f32 = 0.0;
                        let mut got: usize = 0;
                        for _ in 0..channels {
                            if let Ok(s) = consumer.pop() {
                                sum += s;
                                got += 1;
                            }
                        }
                        if got > 0 {
                            written += got as u64;
                            let mono_sample = (sum / channels as f32) * volume * norm_gain;
                            for slot in frame.iter_mut() {
                                *slot = T::from_sample(mono_sample);
                            }
                        } else {
                            for slot in frame.iter_mut() {
                                *slot = T::from_sample(0.0_f32);
                            }
                        }
                    }
                } else {
                    // Normal stereo/multi-channel path.
                    for slot in out.iter_mut() {
                        let sample = match consumer.pop() {
                            Ok(s) => {
                                written += 1;
                                s
                            }
                            Err(_) => 0.0,
                        };
                        *slot = T::from_sample(sample * volume * norm_gain);
                    }
                }

                if written > 0 {
                    shared.samples_played.fetch_add(written, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )
        .map_err(|e| AppError::Audio(format!("build_output_stream: {e}")))?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::fill_pcm_period;
    use crate::audio::state::SharedPlayback;
    use rtrb::RingBuffer;

    #[test]
    fn a_dry_ring_yields_silence_that_is_not_credited() {
        // The counter drives the progress bar and play crediting: an
        // underrun must leave the track where it was, not advance it.
        let shared = SharedPlayback::new();
        let (_producer, mut consumer) = RingBuffer::<f32>::new(8);
        let mut samples = [1.0_f32; 4];
        let written = fill_pcm_period(&shared, &mut consumer, &mut samples, 2);
        assert_eq!(written, 0);
        assert_eq!(samples, [0.0; 4]);
    }

    #[test]
    fn volume_is_applied_here_because_a_raw_device_has_no_mixer() {
        let shared = SharedPlayback::new();
        shared.set_volume(0.5);
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(8);
        for _ in 0..4 {
            producer.push(1.0).expect("ring has room");
        }
        let mut samples = [0.0_f32; 4];
        let written = fill_pcm_period(&shared, &mut consumer, &mut samples, 2);
        assert_eq!(written, 4);
        assert_eq!(samples, [0.5; 4]);
    }

    #[test]
    fn the_mono_downmix_averages_the_frame_across_every_channel() {
        let shared = SharedPlayback::new();
        shared
            .mono_enabled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(8);
        // One stereo frame, hard-panned left.
        producer.push(1.0).expect("ring has room");
        producer.push(0.0).expect("ring has room");
        let mut samples = [0.0_f32; 2];
        let written = fill_pcm_period(&shared, &mut consumer, &mut samples, 2);
        assert_eq!(written, 2);
        assert_eq!(samples, [0.5, 0.5]);
    }
}

#[cfg(test)]
mod alsa_hint_tests {
    use super::{present_alsa_hints, AlsaHintRow};

    fn row(id: &str, display: &str) -> AlsaHintRow {
        AlsaHintRow {
            id: id.to_string(),
            display: display.to_string(),
        }
    }

    fn ids(rows: &[AlsaHintRow]) -> Vec<&str> {
        rows.iter().map(|row| row.id.as_str()).collect()
    }

    #[test]
    fn routing_families_are_not_devices() {
        // The reported list: one card, six lines, and three of them go
        // through the mixer — picking one defeats the exclusive output
        // the user has just turned on.
        let kept = present_alsa_hints(vec![
            row("hw:CARD=PCH,DEV=0", "Built-in Audio"),
            row("plughw:CARD=PCH,DEV=0", "Built-in Audio"),
            row("dmix:CARD=PCH,DEV=0", "Built-in Audio"),
            row("dsnoop:CARD=PCH,DEV=0", "Built-in Audio"),
            row("surround40:CARD=PCH,DEV=0", "Built-in Audio"),
            row("null", "Discard all samples"),
        ]);
        assert_eq!(ids(&kept), ["hw:CARD=PCH,DEV=0"]);
    }

    #[test]
    fn a_wrapper_over_hardware_we_already_show_goes() {
        // `front:` and `sysdefault:` reach the same endpoint as the `hw:`
        // row beside them, and `hw:` is the spelling exclusive output can
        // use. The card token is what pairs them: the names share nothing.
        let kept = present_alsa_hints(vec![
            row("sysdefault:CARD=PCH", "Built-in Audio"),
            row("front:CARD=PCH,DEV=0", "Built-in Audio, Front speakers"),
            row(
                "hw:CARD=PCH,DEV=0",
                "Built-in Audio, Direct hardware device",
            ),
        ]);
        assert_eq!(ids(&kept), ["hw:CARD=PCH,DEV=0"]);
    }

    #[test]
    fn a_wrapper_with_no_hardware_row_behind_it_stays() {
        // Some configurations list no bare `hw:` entry at all. Dropping
        // the only way to reach a card would be worse than showing an
        // alias.
        let kept = present_alsa_hints(vec![
            row("sysdefault:CARD=PCH", "Built-in Audio"),
            row("front:CARD=PCH,DEV=0", "Built-in Audio, Front speakers"),
        ]);
        assert_eq!(ids(&kept), ["sysdefault:CARD=PCH", "front:CARD=PCH,DEV=0"]);
    }

    #[test]
    fn a_different_device_on_the_same_card_is_a_different_output() {
        // S/PDIF and HDMI live on the same card under their own device
        // numbers. Keying on the card alone would hide them behind the
        // analogue output.
        let kept = present_alsa_hints(vec![
            row("hw:CARD=PCH,DEV=0", "Analogue"),
            row("iec958:CARD=PCH,DEV=1", "S/PDIF"),
            row("hdmi:CARD=HDMI,DEV=0", "HDMI 1"),
            row("hdmi:CARD=HDMI,DEV=1", "HDMI 2"),
        ]);
        assert_eq!(
            ids(&kept),
            [
                "hw:CARD=PCH,DEV=0",
                "iec958:CARD=PCH,DEV=1",
                "hdmi:CARD=HDMI,DEV=0",
                "hdmi:CARD=HDMI,DEV=1"
            ]
        );
    }

    #[test]
    fn the_choices_that_are_not_hardware_stay() {
        // `default`, `pulse` and `pipewire` are not devices, and they are
        // the right answer for most people most of the time — on a
        // PipeWire desktop, often the only one that works.
        let kept = present_alsa_hints(vec![
            row("default", "Default Audio Device"),
            row("pulse", "PulseAudio Sound Server"),
            row("pipewire", "PipeWire Sound Server"),
        ]);
        assert_eq!(ids(&kept), ["default", "pulse", "pipewire"]);
    }

    #[test]
    fn two_devices_on_one_card_are_told_apart_by_their_names() {
        // The card token is no help here — both rows carry it — so the
        // suffix falls back to the id, which is unique by construction.
        // Before, the list read "USB Audio (PCH)" twice, which is the
        // duplicate this pass exists to remove.
        let kept = present_alsa_hints(vec![
            row("hw:CARD=PCH,DEV=0", "USB Audio"),
            row("hw:CARD=PCH,DEV=2", "USB Audio"),
        ]);
        assert_eq!(
            kept.iter().map(|r| r.display.as_str()).collect::<Vec<_>>(),
            [
                "USB Audio (hw:CARD=PCH,DEV=0)",
                "USB Audio (hw:CARD=PCH,DEV=2)"
            ]
        );
    }

    #[test]
    fn two_identical_cards_do_not_read_as_one() {
        // Two of the same DAC describe themselves identically. The ids
        // differ, so both picks work — but the list showed one line
        // twice, which reads as a duplicate rather than as two devices.
        let kept = present_alsa_hints(vec![
            row("hw:CARD=D50s,DEV=0", "Topping D50s"),
            row("hw:CARD=D50s_1,DEV=0", "Topping D50s"),
        ]);
        assert_eq!(
            kept.iter().map(|r| r.display.as_str()).collect::<Vec<_>>(),
            ["Topping D50s (D50s)", "Topping D50s (D50s_1)"]
        );
    }
}
