//! What an output device says it can do (#593).
//!
//! The picker lists names, and someone choosing between three outputs on
//! an audiophile player is choosing on facts the names do not carry: the
//! rates the device takes, in which formats, over how many channels. We
//! already learn all of that during exclusive negotiation —
//! `is_supported_exclusive_with_quirks` walks layout × format, ALSA's
//! `hw:` params can be queried, CoreAudio reports what its streams run
//! at — and then throw it away.
//!
//! Two rules shape everything here.
//!
//! **Probe on demand, never during enumeration.** The Linux device list
//! deliberately reads ALSA's hint database rather than opening every PCM,
//! because opening them costs a one-to-two second freeze. Filling a
//! capability table while enumerating would reintroduce exactly that
//! stall, for every device, every time the menu opens. So this is a
//! separate command, called for one device at a time, and memoised.
//!
//! **Say which question was asked.** What a driver *declares* and what it
//! will *accept in exclusive mode* are different questions, and a
//! shared-mode path can advertise rates it reaches by resampling.
//! [`CapabilitySource`] carries which one this answer came from, and the
//! UI states it — that is the difference between an informative sheet and
//! a misleading one.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The rates worth asking a device about: the CD and DVD families, and
/// the doublings up to DSD-over-PCM territory. Not a continuum — a
/// device that accepts 44 100 and 48 000 accepts nothing between them,
/// and asking about 100 values would multiply the probe cost for
/// nothing.
pub const PROBE_RATES: &[u32] = &[
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
];

/// Which question the answer came from, stated rather than implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilitySource {
    /// Windows: each (format, rate) pair was offered to the driver in
    /// **exclusive** mode and accepted or refused. The strongest answer
    /// of the three — it is the same call the real open makes.
    WasapiExclusive,
    /// Linux: ALSA's hardware parameters for the raw `hw:` device, which
    /// describe what the hardware itself takes, with no plug layer in
    /// between.
    AlsaHardware,
    /// macOS: the physical stream formats and nominal rates the device
    /// reports to the HAL. What the device says it runs at, which is not
    /// quite the same as an exclusive-mode acceptance test.
    CoreAudio,
    /// Nothing could be asked: the device is held by someone else, the
    /// name no longer resolves, or this platform has no way to ask.
    Unavailable,
}

/// One format a device accepts, as the driver describes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeviceFormat {
    /// The name the backends already use in their logs: `S24_3LE`,
    /// `F32`, … Kept verbatim so a bug report and the UI say the same
    /// thing.
    pub label: String,
    /// Bits of real audio per sample — 24 for both 24-bit layouts, since
    /// the padding is not resolution.
    pub bits: u16,
    pub float: bool,
    /// The rates accepted **in this format**, ascending.
    ///
    /// Per format rather than per device, because the two are not
    /// independent: a DAC that takes 32-bit up to 96 kHz and 24-bit up
    /// to 192 kHz accepts neither "32-bit at 192 kHz" nor anything else
    /// the two maxima would suggest. A summary built from separate
    /// maxima would name a pair the device has never accepted.
    pub sample_rates: Vec<u32>,
}

/// What one output device accepts (#593).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeviceCapabilities {
    /// The device this describes, `None` for the system default.
    pub device_id: Option<String>,
    pub source: CapabilitySource,
    /// Accepted formats, best first, as the backend's own fallback chain
    /// orders them.
    pub formats: Vec<DeviceFormat>,
    /// Every rate accepted in **some** format, ascending — the union of
    /// the per-format lists above, which is the right set for the tiers
    /// the sheet groups by and the wrong one for naming a pair.
    pub sample_rates: Vec<u32>,
    pub max_channels: u16,
    /// The **smallest period the device will take**, in frames.
    ///
    /// One quantity, asked the same way of every backend, because a
    /// sheet that showed the minimum on one platform and the maximum on
    /// another would invite a comparison that means nothing. This one is
    /// the device's latency floor, which is what an audiophile sheet is
    /// being asked for. `None` where the platform does not answer it —
    /// CoreAudio's buffer size is the client's to choose rather than the
    /// device's to declare.
    pub min_period_frames: Option<u32>,
    /// Why the answer is empty. Technical, for the tooltip and the bug
    /// report — the UI says it in words of its own, like every other
    /// backend message the user can see (#597).
    pub unavailable_reason: Option<String>,
}

impl DeviceCapabilities {
    /// The answer for a device we could not ask.
    pub fn unavailable(device_id: Option<String>, reason: String) -> Self {
        Self {
            device_id,
            source: CapabilitySource::Unavailable,
            formats: Vec::new(),
            sample_rates: Vec::new(),
            max_channels: 0,
            min_period_frames: None,
            unavailable_reason: Some(reason),
        }
    }

    /// Whether this is worth keeping in the memo below: a device that was
    /// busy this time can perfectly well answer the next.
    fn is_answer(&self) -> bool {
        self.source != CapabilitySource::Unavailable
    }
}

/// Memo of what each device answered, for the session.
///
/// A process-wide static rather than engine state, deliberately: these
/// are facts about hardware, not about the playing session or the
/// profile, and the engine is rebuilt far more often than a sound card
/// changes what it accepts. Only real answers are kept — see
/// [`DeviceCapabilities::is_answer`] — so a device that was busy when
/// first asked is asked again rather than remembered as "unavailable"
/// for the rest of the run.
static MEMO: OnceLock<Mutex<HashMap<String, DeviceCapabilities>>> = OnceLock::new();

fn memo() -> &'static Mutex<HashMap<String, DeviceCapabilities>> {
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The key a device is memoised under. The empty pin means "the system
/// default", which is a device like any other for this purpose.
fn memo_key(device_id: Option<&str>) -> String {
    device_id.unwrap_or("<default>").to_string()
}

/// Ask one device what it accepts (#593).
///
/// Blocking — it opens or queries hardware — so call it from a blocking
/// context, never from the audio callback and never in a loop over every
/// device at enumeration time.
///
/// Never fails: a device that cannot be asked comes back as
/// [`CapabilitySource::Unavailable`] with the reason attached, because
/// "we could not ask" is itself an answer the sheet has to be able to
/// show.
pub fn probe_output_device(device_id: Option<&str>) -> DeviceCapabilities {
    let key = memo_key(device_id);
    if let Some(cached) = memo().lock().ok().and_then(|memo| memo.get(&key).cloned()) {
        return cached;
    }

    let probed = probe_uncached(device_id);
    if probed.is_answer() {
        if let Ok(mut memo) = memo().lock() {
            memo.insert(key, probed.clone());
        }
    }
    probed
}

#[cfg_attr(
    not(any(target_os = "windows", target_os = "linux", target_os = "macos")),
    allow(unused_variables)
)]
fn probe_uncached(device_id: Option<&str>) -> DeviceCapabilities {
    #[cfg(target_os = "windows")]
    {
        super::wasapi_exclusive::probe_capabilities(device_id).unwrap_or_else(|err| {
            DeviceCapabilities::unavailable(device_id.map(str::to_string), err.to_string())
        })
    }
    #[cfg(target_os = "linux")]
    {
        super::alsa_exclusive::probe_capabilities(device_id).unwrap_or_else(|err| {
            DeviceCapabilities::unavailable(device_id.map(str::to_string), err.to_string())
        })
    }
    #[cfg(target_os = "macos")]
    {
        super::coreaudio_exclusive::probe_capabilities(device_id).unwrap_or_else(|err| {
            DeviceCapabilities::unavailable(device_id.map(str::to_string), err.to_string())
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        DeviceCapabilities::unavailable(
            device_id.map(str::to_string),
            "no capability probe on this platform".to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_we_could_not_ask_is_not_remembered_as_such() {
        // The one caching rule that matters: a device held by another
        // client answers nothing *now*. Remembering that would make the
        // sheet say "unavailable" for the rest of the session, long after
        // the other client let go.
        let busy = DeviceCapabilities::unavailable(Some("hw:CARD=D50s".into()), "busy".into());
        assert!(!busy.is_answer());
    }

    #[test]
    fn a_real_answer_is_remembered() {
        let answered = DeviceCapabilities {
            device_id: None,
            source: CapabilitySource::WasapiExclusive,
            formats: vec![DeviceFormat {
                label: "S24_4LE".into(),
                bits: 24,
                float: false,
                sample_rates: vec![44_100, 48_000],
            }],
            sample_rates: vec![44_100, 48_000],
            max_channels: 2,
            min_period_frames: Some(480),
            unavailable_reason: None,
        };
        assert!(answered.is_answer());
    }

    #[test]
    fn the_default_device_has_a_key_of_its_own() {
        assert_ne!(memo_key(None), memo_key(Some("hw:CARD=PCH,DEV=0")));
    }
}
