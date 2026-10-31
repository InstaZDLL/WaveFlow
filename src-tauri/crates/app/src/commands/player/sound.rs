//! The sound settings: volume, normalization, DSD, ReplayGain, channel mix,
//! crossfade, visualizer, speed and gapless, and the snapshot of all of
//! them.

use std::sync::Arc;

use serde::Serialize;
use tauri::AppHandle;

use crate::{
    audio::{engine::AudioCmd, replay_gain::GainMode, AudioEngine},
    error::AppResult,
    state::AppState,
};

use super::*;

/// New volume after a `player_set_volume`, as `0.0..=1.0` — the same
/// scale as the `volume` field of the player snapshot.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct VolumeChangedPayload {
    pub(crate) volume: f32,
}

/// Set playback volume. `value` is clamped to `[0.0, 1.0]` on the
/// decoder side; values outside that range are saturated, not rejected.
/// Also persists to `profile_setting['player.volume']` (as an int 0-100
/// rounded from the f32) so the volume survives an app restart.
///
/// The sequence lives in [`crate::player_actions::set_volume`], which the
/// MPD server and the global shortcuts drive as well.
#[tauri::command]
pub async fn player_set_volume(app: AppHandle, value: f32) -> AppResult<()> {
    crate::player_actions::set_volume(&app, value).await?;
    Ok(())
}

/// Toggle volume normalization (−3 dB gain reduction on loud tracks).
/// Persisted in `profile_setting['audio.normalize']`.
#[tauri::command]
pub async fn player_set_normalize(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.normalize",
            if enabled { "true" } else { "false" },
            SettingType::Bool,
        )
        .await?;
    }
    engine.send(AudioCmd::SetNormalize(enabled))?;
    Ok(())
}

/// Allowed DSD → PCM FIR tap counts. 256 is the historical default
/// (~92 dB stop-band); 1024 / 2048 give a sharper transition band for
/// audiophile DSD playback at a linear convolution cost. The decoder
/// clamps anything else up to 64, but the command only persists a value
/// from this set so the UI and the engine never disagree.
pub(super) const DSD_TAPS_ALLOWED: [u32; 3] = [256, 1024, 2048];

/// Set the DSD → PCM converter precision (FIR tap count). Only affects
/// `.dsf` / `.dff` playback — symphonia formats ignore it. Takes effect
/// on the next track open. Persisted in `profile_setting['audio.dsd_precision']`.
#[tauri::command]
pub async fn player_set_dsd_precision(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    taps: u32,
) -> AppResult<()> {
    // Reject an out-of-set value rather than silently clamping — keeps
    // the persisted row in lockstep with the three UI options.
    let taps = if DSD_TAPS_ALLOWED.contains(&taps) {
        taps
    } else {
        256
    };
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.dsd_precision",
            taps.to_string(),
            SettingType::Int,
        )
        .await?;
    }
    engine
        .shared()
        .dsd_taps
        .store(taps, std::sync::atomic::Ordering::Release);
    Ok(())
}

/// Toggle native DSD output via DoP (DSD over PCM), #495. When on AND an
/// exclusive output can be opened — WASAPI Exclusive (Windows), a raw
/// ALSA `hw:` device (Linux), CoreAudio hog mode (macOS) — AND the DAC
/// accepts the DoP format, a `.dsf` / `.dff` track is shipped as raw
/// 1-bit DoP frames instead of being converted to PCM — the DAC decodes
/// the DSD natively. Any of those conditions failing falls back silently
/// to DSD → PCM, so it's safe to leave on. Takes effect on the next
/// track open. Persisted in `profile_setting['audio.dsd_dop']`,
/// default OFF.
#[tauri::command]
pub async fn player_set_dsd_dop(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.dsd_dop",
            if enabled { "true" } else { "false" },
            SettingType::Bool,
        )
        .await?;
    }
    engine
        .shared()
        .dsd_dop_enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
    Ok(())
}

/// Toggle ReplayGain — multiply each track by its analyzed gain to
/// even out perceived loudness across the library.
/// Persisted in `profile_setting['audio.replaygain']`.
#[tauri::command]
pub async fn player_set_replaygain(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.replaygain",
            if enabled { "true" } else { "false" },
            SettingType::Bool,
        )
        .await?;
    }
    engine.send(AudioCmd::SetReplayGain(enabled))?;
    Ok(())
}

/// Bound on the pre-amp and on the fallback gain, in dB. Wide enough
/// for any real listening preference, narrow enough that a
/// hand-edited database can't hand the mixer a 40 dB multiplier.
pub const REPLAYGAIN_ADJUST_LIMIT_DB: f32 = 15.0;

/// Set the three ReplayGain knobs that sit on top of the on/off
/// switch: pre-amp, fallback gain for tracks nothing knows about, and
/// clipping prevention.
///
/// Written straight into `SharedPlayback` rather than sent as an
/// [`AudioCmd`] — they are plain atomics the decoder re-reads on every
/// buffer, so there is no ordering to arrange with the decode stream,
/// and this is the same path the boot-time restore takes.
///
/// Persisted in `profile_setting['audio.replaygain_preamp' |
/// 'audio.replaygain_fallback' | 'audio.replaygain_prevent_clipping']`.
#[tauri::command]
pub async fn player_set_replaygain_options(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    preamp_db: f32,
    fallback_db: f32,
    prevent_clipping: bool,
) -> AppResult<()> {
    let preamp = clamp_replaygain_adjust(preamp_db);
    let fallback = clamp_replaygain_adjust(fallback_db);

    if let Ok(pool) = state.require_profile_pool().await {
        // The two gains are decimals, which the table has no marker for:
        // stored as text like `ui.zoom_level`, and parsed back on restore.
        // They were written as `'float'`, which the CHECK refuses, so
        // neither ever survived a restart.
        for (key, value, value_type) in [
            (
                "audio.replaygain_preamp",
                preamp.to_string(),
                SettingType::Text,
            ),
            (
                "audio.replaygain_fallback",
                fallback.to_string(),
                SettingType::Text,
            ),
            (
                "audio.replaygain_prevent_clipping",
                prevent_clipping.to_string(),
                SettingType::Bool,
            ),
        ] {
            persist_setting(&pool, key, value, value_type).await?;
        }
    }

    let shared = engine.shared();
    shared
        .replaygain_preamp_db_bits
        .store(preamp.to_bits(), std::sync::atomic::Ordering::Relaxed);
    shared
        .replaygain_fallback_db_bits
        .store(fallback.to_bits(), std::sync::atomic::Ordering::Relaxed);
    shared
        .replaygain_prevent_clipping
        .store(prevent_clipping, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Choose which of a file's two ReplayGain measurements to apply:
/// `"track"`, `"album"`, or `"auto"` (album gain while a record plays
/// through, track gain otherwise).
///
/// Written straight into `SharedPlayback` like the other knobs — the
/// decoder re-reads it per buffer, so the change is audible on the
/// track already playing rather than at the next one.
///
/// An unrecognised value resolves to `auto` rather than failing: this
/// is a preference, and the only thing worse than the wrong mode is no
/// sound while the frontend and the backend argue about a string.
///
/// Persisted in `profile_setting['audio.replaygain_mode']`.
#[tauri::command]
pub async fn player_set_replaygain_mode(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    mode: String,
) -> AppResult<()> {
    let mode = GainMode::from_setting(&mode);
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.replaygain_mode",
            mode.as_setting(),
            SettingType::Text,
        )
        .await?;
    }
    engine
        .shared()
        .replaygain_mode_bits
        .store(mode.as_bits(), std::sync::atomic::Ordering::Relaxed);

    Ok(())
}

/// Keep a pre-amp / fallback value inside the supported range, and
/// turn a non-finite one into a no-op rather than into silence.
pub(super) fn clamp_replaygain_adjust(db: f32) -> f32 {
    if db.is_finite() {
        db.clamp(-REPLAYGAIN_ADJUST_LIMIT_DB, REPLAYGAIN_ADJUST_LIMIT_DB)
    } else {
        0.0
    }
}

/// Toggle mono downmix (average L+R into both channels).
/// Persisted in `profile_setting['audio.mono']`.
#[tauri::command]
pub async fn player_set_mono(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.mono",
            if enabled { "true" } else { "false" },
            SettingType::Bool,
        )
        .await?;
    }
    engine.send(AudioCmd::SetMono(enabled))?;
    Ok(())
}

/// Set the left / right balance, in percent: `-100` is hard left, `100`
/// hard right, `0` the centre. Saved first, then applied; a value the
/// database refuses never reaches the engine. Persisted in
/// `profile_setting['audio.balance']` as an int.
#[tauri::command]
pub async fn player_set_balance(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    value: i32,
) -> AppResult<()> {
    let limit = i32::from(crate::audio::stereo::BALANCE_LIMIT);
    let clamped = value.clamp(-limit, limit);
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.balance",
            clamped.to_string(),
            SettingType::Int,
        )
        .await?;
    }
    engine.send(AudioCmd::SetBalance(clamped))?;
    Ok(())
}

/// Set the stereo width, in percent: `0` is mono, `100` the source as it
/// is, `200` the side doubled. Saved first, then applied, like the
/// balance. Persisted in `profile_setting['audio.stereo_width']` as an
/// int.
#[tauri::command]
pub async fn player_set_stereo_width(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    value: i32,
) -> AppResult<()> {
    let clamped = value.clamp(0, i32::from(crate::audio::stereo::WIDTH_MAX));
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.stereo_width",
            clamped.to_string(),
            SettingType::Int,
        )
        .await?;
    }
    engine.send(AudioCmd::SetStereoWidth(clamped))?;
    Ok(())
}

/// Toggle smart crossfade. When ON, same-album transitions skip
/// the fade and fall through to the gapless hand-off so concept
/// albums / live records aren't smeared by an equal-power mix.
/// Default OFF — it's an opinionated behaviour change so users
/// opt in. Persisted in `profile_setting['audio.smart_crossfade']`.
#[tauri::command]
pub async fn player_set_smart_crossfade(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.smart_crossfade",
            if enabled { "true" } else { "false" },
            SettingType::Bool,
        )
        .await?;
    }
    engine
        .shared()
        .smart_crossfade_enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
    Ok(())
}

#[tauri::command]
pub async fn player_get_smart_crossfade(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<bool> {
    Ok(engine
        .shared()
        .smart_crossfade_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Toggle dynamic (tempo-aware) crossfade. When ON, the analytics
/// worker scales each upcoming fade by the BPM gap between the
/// current and next tracks: similar tempos keep the full window
/// (clean blend), large gaps shrink it so the rhythms don't clash.
/// Falls through to the user's static `crossfade_ms` when either
/// track has no stored BPM. Default OFF — opt-in via Settings.
/// Persisted in `profile_setting['audio.dynamic_crossfade']`.
#[tauri::command]
pub async fn player_set_dynamic_crossfade(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.dynamic_crossfade",
            if enabled { "true" } else { "false" },
            SettingType::Bool,
        )
        .await?;
    }
    engine
        .shared()
        .dynamic_crossfade_enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
    if !enabled {
        // Drop any in-flight override so the next transition snaps
        // back to the static crossfade immediately.
        engine
            .shared()
            .pending_next_crossfade_ms
            .store(0, std::sync::atomic::Ordering::Release);
    }
    Ok(())
}

#[tauri::command]
pub async fn player_get_dynamic_crossfade(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<bool> {
    Ok(engine
        .shared()
        .dynamic_crossfade_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Toggle AutoMix: the crossfade placed on both tracks' beats, its length
/// counted in beats within the crossfade setting, falling back to the
/// smart crossfade when a track has no usable beat grid or the tempos are
/// too far apart. Same-album transitions play through without a fade, as
/// with smart crossfade. Default OFF. Persisted in
/// `profile_setting['audio.automix']`.
#[tauri::command]
pub async fn player_set_automix(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.automix",
            if enabled { "true" } else { "false" },
            SettingType::Bool,
        )
        .await?;
    }
    let automix = &engine.shared().automix;
    automix
        .enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
    if !enabled {
        // A plan already handed over would still be honoured: drop it
        // so the next transition is the plain one at once.
        automix.clear_plan();
    }
    Ok(())
}

#[tauri::command]
pub async fn player_get_automix(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<bool> {
    Ok(engine.shared().automix.is_enabled())
}

/// Toggle the spectrum visualizer. Flips the atomic the decoder
/// thread checks before running the FFT, then persists the new
/// value to `profile_setting['ui.visualizer']` so the choice
/// survives a restart.
#[tauri::command]
pub async fn player_set_visualizer(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "ui.visualizer",
            if enabled { "true" } else { "false" },
            SettingType::Bool,
        )
        .await?;
    }
    engine
        .shared()
        .visualizer_enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
    Ok(())
}

#[tauri::command]
pub async fn player_get_visualizer(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<bool> {
    Ok(engine
        .shared()
        .visualizer_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Update the playback speed multiplier. Pushes the new value live
/// to the audio engine so the active stream's resampler is rebuilt
/// against the new effective input rate, then persists to
/// `profile_setting['audio.playback_speed']`. Clamped to `[0.5, 2.0]`
/// on the engine side — values outside that range are saturated, not
/// rejected.
///
/// Pitch is NOT preserved (1.5× → ~ +7 semitones). Proper pitch-locked
/// time-stretching would need a phase vocoder; this is intentionally
/// the simple resampler-shift approach used by VLC's default playback
/// rate, since it costs ~zero CPU and works on every codec.
#[tauri::command]
pub async fn player_set_speed(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    value: f32,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        let clamped = value.clamp(0.5, 2.0);
        persist_setting(
            &pool,
            "audio.playback_speed",
            clamped.to_string(),
            SettingType::Text,
        )
        .await?;
    }
    engine.send(AudioCmd::SetSpeed(value))?;
    Ok(())
}

#[tauri::command]
pub async fn player_get_speed(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<f32> {
    Ok(engine.shared().playback_speed())
}

/// Toggle gapless playback. Pushes the new value live to the audio
/// engine and persists to `profile_setting['audio.gapless']`.
#[tauri::command]
pub async fn player_set_gapless(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.gapless",
            if enabled { "true" } else { "false" },
            SettingType::Bool,
        )
        .await?;
    }
    engine.send(AudioCmd::SetGapless(enabled))?;
    Ok(())
}

/// Update the crossfade duration. Pushes the new value live to the
/// audio engine so the next track transition uses it, and persists
/// to `profile_setting['audio.crossfade_ms']` so it survives a restart.
#[tauri::command]
pub async fn player_set_crossfade(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    seconds: f64,
) -> AppResult<()> {
    let ms = (seconds.max(0.0) * 1000.0).round() as i64;
    if let Ok(pool) = state.require_profile_pool().await {
        persist_setting(
            &pool,
            "audio.crossfade_ms",
            ms.to_string(),
            SettingType::Int,
        )
        .await?;
    }
    engine.send(AudioCmd::SetCrossfade(ms.max(0) as u32))?;
    Ok(())
}

/// Return current audio settings so the Settings view can hydrate.
#[tauri::command]
pub async fn player_get_audio_settings(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<AudioSettingsSnapshot> {
    let shared = engine.shared();
    let normalize = shared
        .normalize_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let mono = shared
        .mono_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let replaygain = shared
        .replaygain_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let gapless = shared
        .gapless_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let dsd_taps = shared.dsd_taps.load(std::sync::atomic::Ordering::Relaxed);
    let dsd_dop = shared
        .dsd_dop_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let match_source_rate = shared
        .match_source_rate
        .load(std::sync::atomic::Ordering::Relaxed);

    let mut crossfade_ms: i64 = 0;
    if let Ok(pool) = state.require_profile_pool().await {
        if let Ok(Some(val)) = sqlx::query_scalar::<_, String>(
            "SELECT value FROM profile_setting WHERE key = 'audio.crossfade_ms'",
        )
        .fetch_optional(&*pool)
        .await
        {
            crossfade_ms = val.parse().unwrap_or(0);
        }
    }

    let replay_gain_settings = shared.replay_gain_settings();
    // No profile loaded reads as the default, like the other rows here.
    let keep_awake = match state.require_profile_pool().await {
        Ok(pool) => read_bool_setting(&pool, "playback.keep_awake", true).await,
        Err(_) => true,
    };

    Ok(AudioSettingsSnapshot {
        normalize,
        mono,
        balance: shared.stereo.balance(),
        stereo_width: shared.stereo.width(),
        crossfade_ms,
        replaygain,
        replaygain_preamp_db: replay_gain_settings.preamp_db as f32,
        replaygain_fallback_db: replay_gain_settings.fallback_db as f32,
        replaygain_prevent_clipping: replay_gain_settings.prevent_clipping,
        replaygain_mode: replay_gain_settings.mode.as_setting().to_string(),
        gapless,
        dsd_taps,
        dsd_dop,
        match_source_rate,
        pause_on_device_loss: engine.pause_on_device_loss(),
        keep_awake,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct AudioSettingsSnapshot {
    pub normalize: bool,
    pub mono: bool,
    /// Left / right balance in percent, `-100` to `100`.
    pub balance: i16,
    /// Stereo width in percent, `0` (mono) to `200`; `100` is neutral.
    pub stereo_width: i16,
    pub crossfade_ms: i64,
    pub replaygain: bool,
    /// Pre-amp added to every track's ReplayGain, in dB.
    pub replaygain_preamp_db: f32,
    /// Gain used for tracks that carry none and were never analysed.
    pub replaygain_fallback_db: f32,
    /// Hold gains back to the headroom each track's peak leaves.
    pub replaygain_prevent_clipping: bool,
    /// `"track"` / `"album"` / `"auto"` — see [`GainMode`].
    pub replaygain_mode: String,
    pub gapless: bool,
    /// Active DSD → PCM FIR tap count (256 / 1024 / 2048).
    pub dsd_taps: u32,
    /// Native DSD via DoP opt-in (#495), default false.
    pub dsd_dop: bool,
    /// Open the output at each track's own rate (#600), default false.
    pub match_source_rate: bool,
    /// Park playback when the output device goes away (#617), default
    /// true.
    pub pause_on_device_loss: bool,
    /// Keep the machine from sleeping while playing (`power.rs`), default
    /// true.
    pub keep_awake: bool,
}
