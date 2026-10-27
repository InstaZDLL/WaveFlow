//! The output device: listing, choosing, reopening and probing it, the
//! exclusive mode, and the device-loss and keep-awake options.

use std::sync::Arc;

use serde::Serialize;

use crate::{
    audio::AudioEngine,
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

/// One row in the output-device picker that powers the PlayerBar
/// device menu. `id` and `name` are both the cpal device name (cpal
/// gives us no stable ID, and the UI matches selections by name on
/// the way back). `is_active` is true for the device the engine is
/// currently driving — `None` in `current_output_device` means
/// "tracking the OS default", which is selected if no row matches.
#[derive(Debug, serde::Serialize)]
pub struct OutputDeviceRow {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    /// The device audio really comes out of, read from what the backend
    /// opened rather than what it was asked for. A pinned device that is
    /// no longer enumerated is replaced by the default endpoint on open,
    /// and flagging the pin then described a device playing nothing
    /// (#612).
    pub is_active: bool,
    /// The device the user picked, whether or not it could be opened.
    /// Differs from [`Self::is_active`] exactly during such a fallback,
    /// which is what lets the picker say so instead of hiding it.
    pub is_pinned: bool,
}

/// Enumerate every output device and flag which one the engine is
/// currently driving. The frontend re-fetches this whenever the
/// device menu opens so freshly-attached USB DACs / Bluetooth sinks
/// show up without an app restart.
///
/// On Linux ALSA, `output_devices()` probes every card and prints
/// scary `pcm_dmix` / `pcm_route` warnings for cards that are
/// enumerable but not openable (HDMI sinks with no monitor attached,
/// Bluetooth profiles in the wrong state, …). The errors don't
/// indicate a real problem on our side — they're cpal internals
/// leaking through ALSA's stderr. We still return whatever names
/// cpal hands us; clicking a broken device is handled by the engine
/// keeping the previous device alive.
#[tauri::command]
pub async fn player_list_output_devices(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<Vec<OutputDeviceRow>> {
    // Two different questions: where the audio actually goes, and what
    // the user asked for. They diverge after a silent fallback (#612),
    // and they are read under one lock — taken separately, a rebuild
    // landing in between would pair one stream's endpoint with the
    // other's pin.
    let (playing, pinned) = engine.current_output_devices();
    // cpal enumeration walks the OS audio stack and can block for
    // ~100 ms+ on Linux. Push it onto the blocking pool so the tokio
    // runtime stays responsive — without this the WebView freezes
    // while the menu is opening.
    let devices = tokio::task::spawn_blocking(crate::audio::list_output_devices)
        .await
        .map_err(|e| AppError::Audio(format!("device enumeration task: {e}")))??;
    let mut rows: Vec<OutputDeviceRow> = devices
        .into_iter()
        .map(|d| {
            // When the engine isn't pinned to a specific device
            // (`active = None`), it's tracking the OS default — so
            // show the OS-default row as active. Without this the
            // header reads "Active output" with nothing highlighted
            // on first open, which looks broken even though playback
            // works fine.
            // Prefer what actually opened. A backend that cannot name it
            // leaves the pin as the best answer available, and with no
            // pin either the engine is tracking the OS default.
            let is_active = match playing.as_deref() {
                Some(name) => d.id == name,
                None => match pinned.as_deref() {
                    Some(name) => d.id == name,
                    None => d.is_default,
                },
            };
            OutputDeviceRow {
                is_active,
                is_pinned: pinned.as_deref().is_some_and(|name| d.id == name),
                id: d.id,
                name: d.name,
                is_default: d.is_default,
            }
        })
        .collect();
    // A pin that is no longer enumerated matches no row, so without this
    // the list carries no `is_pinned` at all — in precisely the fallback
    // case this is meant to report. Add the absent device back as its own
    // row so the menu can say it is unavailable rather than stay silent
    // about it (#612). It is never `is_active`: it isn't there to play.
    if let Some(name) = pinned.as_deref() {
        if !rows.iter().any(|r| r.id == name) {
            rows.push(OutputDeviceRow {
                id: name.to_string(),
                name: name.to_string(),
                is_default: false,
                is_active: false,
                is_pinned: true,
            });
        }
    }
    Ok(rows)
}

/// Switch playback to a different cpal output device. `device_id =
/// None` follows the OS default (the engine's startup state). The
/// engine validates the new device, releases the old one, then
/// resumes the same track at the same position. If the new device
/// can't be opened (broken HDMI sink, busy exclusive output, …) the
/// engine keeps the previous device active and the error is
/// surfaced to the frontend.
///
/// Persisted in `profile_setting['audio.output_device']` so the
/// choice survives an app restart.
#[tauri::command]
pub async fn player_set_output_device(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    device_id: Option<String>,
) -> AppResult<()> {
    let engine_clone: Arc<AudioEngine> = engine.inner().clone();
    let device_for_engine = device_id.clone();
    // Same rationale as `player_list_output_devices`: opening /
    // tearing down a cpal stream blocks. Don't wedge the runtime.
    tokio::task::spawn_blocking(move || engine_clone.set_output_device(device_for_engine))
        .await
        .map_err(|e| AppError::Audio(format!("set output device task: {e}")))??;
    if let Ok(pool) = state.require_profile_pool().await {
        // Empty string represents "default" so the column stays
        // NOT NULL — the load path translates back to None.
        let stored = device_id.unwrap_or_default();
        // After the switch, not before: opening the device is what tells a
        // usable choice from one that is not, and only a usable one should
        // be tried again at the next launch. A failure here leaves the
        // session on the device it switched to and says so.
        persist_setting(&pool, "audio.output_device", stored, SettingType::Text).await?;
    }
    Ok(())
}

/// Re-open the output stream on the device that is already selected.
///
/// The picker's active row used to be inert, so a user whose audio had
/// drifted onto another endpoint had to select a different device and
/// then select theirs again to force a real lookup (#612). Clicking the
/// active row lands here instead. The selection itself doesn't change,
/// so nothing is persisted.
#[tauri::command]
pub async fn player_reopen_output_device(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<()> {
    let engine_clone: Arc<AudioEngine> = engine.inner().clone();
    // Same rationale as the two commands above: opening and tearing down
    // a stream blocks, and the runtime must not wedge behind it.
    tokio::task::spawn_blocking(move || engine_clone.reopen_output_device())
        .await
        .map_err(|e| AppError::Audio(format!("reopen output device task: {e}")))?
}

/// Toggle exclusive output — the audiophile path where the app owns
/// the device instead of sharing it with the system mixer.
///
/// Re-opens the active output stream through the platform's exclusive
/// backend:
///
/// - **Windows**, WASAPI Exclusive Mode, event-driven, negotiating the
///   layout from the endpoint's own format
///   (`PKEY_AudioEngine_DeviceFormat`) first and only then the
///   shared-mode mix format (#409) — see `audio/wasapi_exclusive/`.
/// - **Linux**, a raw `hw:` ALSA device, asking the sound server to
///   release the card first — see `audio/alsa_exclusive/`.
/// - **macOS**, CoreAudio hog mode, which leaves the device's physical
///   format alone — see `audio/coreaudio_exclusive.rs`.
///
/// Either way no other app can mix in, DSP or resample our audio. Falls
/// back silently to cpal shared mode if init fails (device busy,
/// unsupported format, no exclusive support in the driver).
///
/// The persisted setting is written on every platform, including those
/// with no exclusive backend at all, so the value follows the user
/// across machines.
///
/// Persisted in `profile_setting['audio.exclusive_output']`. The boot
/// read in `startup/setup.rs` also accepts the legacy `audio.wasapi_exclusive`
/// row, from when this was a Windows-only setting.
#[tauri::command]
pub async fn player_set_exclusive_output(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    // The preference first: it is what the card shows, whether or not the
    // device then engages, and a preference the database refused must not
    // be acted on.
    if let Ok(pool) = state.require_profile_pool().await {
        let stored = if enabled { "1" } else { "0" };
        persist_setting(&pool, "audio.exclusive_output", stored, SettingType::Bool).await?;
    }
    let engine_clone: Arc<AudioEngine> = engine.inner().clone();
    // Same rationale as `player_set_output_device`: opening / tearing
    // down a WASAPI stream blocks for a few hundred ms.
    tokio::task::spawn_blocking(move || engine_clone.set_exclusive_output(enabled))
        .await
        .map_err(|e| AppError::Audio(format!("set exclusive output task: {e}")))??;
    Ok(())
}

/// Read from the audio engine whether the output really owns its
/// device. Used by the Settings card to reflect what actually engaged,
/// since init can have silently fallen back to shared mode.
#[tauri::command]
pub fn player_get_exclusive_output(engine: tauri::State<'_, Arc<AudioEngine>>) -> bool {
    engine.inner().exclusive_output()
}

/// The exclusive-output preference and what it achieved, together.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ExclusiveOutputState {
    /// What the user asked for (`profile_setting['audio.exclusive_output']`).
    pub requested: bool,
    /// Whether the current stream really owns its device.
    pub engaged: bool,
    /// Why it does not, when it was asked to and the backend could tell.
    pub refusal: Option<crate::audio::engine::ExclusiveRefusal>,
}

/// For the Settings toggle, which has to show the preference: showing only
/// what engaged ([`player_get_exclusive_output`]) put the switch at "off"
/// after a refusal while exclusive stayed requested — every rebuild tried
/// the device again, and flipping the switch "on" changed nothing. The
/// card shows `requested` and says so when `engaged` falls short.
#[tauri::command]
pub fn player_get_exclusive_output_state(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> ExclusiveOutputState {
    let engine = engine.inner();
    ExclusiveOutputState {
        requested: engine.exclusive_output_requested(),
        engaged: engine.exclusive_output(),
        refusal: engine.exclusive_refusal(),
    }
}

/// Open the output at each track's own rate instead of taking whatever
/// the device offers (#600).
///
/// With this on, and only while the output really owns its device, a
/// 44.1 kHz track plays at 44.1 and a 96 kHz track at 96, with nothing
/// resampling in between. A device that refuses the track's rate falls
/// back to one it does offer and the decoder's resampler meets it, as it
/// always did — that fallback is what makes this a preference rather
/// than DoP's demand.
///
/// Off by default, and not because it is worse: reopening the device
/// costs an audible gap, so every rate change becomes a break in the
/// music. For most listeners our own resampler, with nothing else mixed
/// in, is the better trade. Takes effect on the next track.
///
/// Persisted in `profile_setting['audio.match_source_rate']`.
#[tauri::command]
pub async fn player_set_match_source_rate(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    // Written before it is applied, and the error propagates. A
    // preference the UI believes it saved, and that comes back to its
    // old value at the next launch or the next profile switch, is worse
    // than a toggle that visibly refuses — and this one takes effect on
    // the next track, so there is nothing to undo in the engine when the
    // write fails.
    let pool = state.require_profile_pool().await?;
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.match_source_rate', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(if enabled { "true" } else { "false" })
    .bind(now)
    .execute(&*pool)
    .await?;
    engine
        .shared()
        .match_source_rate
        .store(enabled, std::sync::atomic::Ordering::Release);
    Ok(())
}

/// What one output device accepts — formats, rates, channels (#593).
///
/// **One device at a time, on demand.** The listing deliberately never
/// asks this: on Linux it reads ALSA's hint database precisely to avoid
/// opening every PCM, and filling a capability table while enumerating
/// would bring back the one-to-two second freeze that shortcut exists to
/// prevent. The answer is memoised for the session, so the second look at
/// the same device is free.
///
/// Blocking work — a COM round trip per format on Windows, an open on
/// Linux — so it runs on the blocking pool. It never fails: a device that
/// cannot be asked comes back with `source: "unavailable"` and the
/// reason, because "we could not ask" is an answer the sheet has to show.
#[tauri::command]
pub async fn player_probe_output_device(
    device_id: Option<String>,
) -> AppResult<crate::audio::capabilities::DeviceCapabilities> {
    let probed = {
        let device_id = device_id.clone();
        tokio::task::spawn_blocking(move || {
            crate::audio::capabilities::probe_output_device(device_id.as_deref())
        })
        .await
    };
    // A panicked or cancelled blocking task is one more way of not being
    // able to ask — so it comes back as the same answer as a busy device
    // rather than as a rejected promise the caller has no branch for.
    Ok(probed.unwrap_or_else(|e| {
        crate::audio::capabilities::DeviceCapabilities::unavailable(
            device_id,
            format!("probe output device task: {e}"),
        )
    }))
}

/// Pause instead of following the music onto another device when the one
/// it is playing on goes away (#617).
///
/// Applies to the next device loss and rebuilds nothing now: the setting
/// describes what to do when something else happens. A device that flaps
/// and comes back is still picked up where it was — only a fallback onto
/// a *different* endpoint parks the session.
///
/// Persisted in `profile_setting['audio.pause_on_device_loss']`, read at
/// boot in `lib.rs`, and on by default: it is what people expect from
/// headphones, and the reported defect is the other behaviour.
#[tauri::command]
pub async fn player_set_pause_on_device_loss(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    // Persisted first, same as `player_set_match_source_rate` and for
    // the same reason: a setting that only ever lived in memory is a
    // setting the user will find undone.
    let pool = state.require_profile_pool().await?;
    let now = chrono::Utc::now().timestamp_millis();
    let stored = if enabled { "1" } else { "0" };
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.pause_on_device_loss', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(stored)
    .bind(now)
    .execute(&*pool)
    .await?;
    engine.set_pause_on_device_loss(enabled);
    Ok(())
}

/// Keep the computer awake while music plays, or let it sleep as usual.
///
/// Persisted in `profile_setting['playback.keep_awake']`, read on every
/// profile load, on by default: a desktop suspending in the middle of an
/// album is the reported defect. It never blocks a suspend the user asks
/// for — see `power.rs`.
#[tauri::command]
pub async fn player_set_keep_awake(
    state: tauri::State<'_, AppState>,
    enabled: bool,
    expected_profile_id: Option<i64>,
) -> AppResult<()> {
    // One write at a time, end to end: two calls interleaving between the
    // SQL write and `set_enabled` could leave the inhibitor on the value
    // that was written first. The settings view already serializes its
    // writes; this keeps the command right for any caller.
    static WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _write = WRITE.lock().await;
    // The profile the toggle was flipped in: checked and leased under the
    // lock a profile switch takes, so a switch in flight cannot redirect
    // the write — or the inhibitor — to the next profile.
    let (pool, written_profile) = state
        .require_profile_pool_with_id(expected_profile_id)
        .await?;
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('playback.keep_awake', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(if enabled { "1" } else { "0" })
    .bind(now)
    .execute(&*pool)
    .await?;
    drop(pool);
    // The lease covered the write; the inhibitor is process-wide and needs
    // its own ordering. Under the profile lock a switch takes for writing:
    // applied while this is still the active profile, or not at all — a
    // switch that got in first has run, or will run, the next profile's
    // own restore, which must not be overwritten with this one's value.
    let active = state.profile.read().await;
    if active.as_ref().map(|p| p.profile_id) == Some(written_profile) {
        crate::power::set_enabled(enabled);
    }
    Ok(())
}

/// Keep the display on while the theater view is open and the music
/// plays, whatever `playback.keep_awake` says — see `power.rs`. Not
/// persisted: the view takes the hold when it enters theater mode and
/// gives it back when it leaves or closes, and the process gives it back
/// when it exits.
#[tauri::command]
pub fn player_hold_display(held: bool) {
    crate::power::set_display_held(held);
}
