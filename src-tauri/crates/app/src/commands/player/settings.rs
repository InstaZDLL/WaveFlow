//! Persisting the player's settings per profile, restoring them into the
//! engine when a profile opens, and telling the windows they changed.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::{
    audio::{replay_gain::GainMode, AudioEngine},
    error::AppResult,
    queue::{self},
};

use super::*;

/// Repeat + shuffle, emitted so every frontend `PlayerContext` reflects an
/// options change — an external surface (an MPD client toggling `repeat` /
/// `random`) has no other way to reach the UI, and neither has the *other*
/// window: `player:state` carries only the play-state, and re-reading
/// `player_get_state` would re-apply every audio setting to the engine as a
/// side effect.
///
/// The in-app buttons still flip optimistically, so the window that pressed
/// one receives an echo of a value it already holds — a no-op. Unlike volume
/// these are discrete toggles with no debounce, so there's no stale echo to
/// guard against (#523).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OptionsChangedPayload {
    repeat_mode: String,
    shuffle: bool,
    /// `"off"` / `"tracks"` / `"albums"`. Sent alongside `shuffle`
    /// rather than instead of it: an MPD client can only ever say
    /// on or off, so the boolean stays the one it drives.
    shuffle_mode: String,
}

pub(crate) async fn emit_options_changed(app: &AppHandle, pool: &sqlx::SqlitePool) {
    let repeat_mode = queue::read_repeat_mode(pool).await.as_str().to_string();
    let mode = queue::read_shuffle_mode(pool).await;
    let _ = app.emit(
        "player:options-changed",
        OptionsChangedPayload {
            repeat_mode,
            shuffle: mode.is_on(),
            shuffle_mode: mode.as_str().to_string(),
        },
    );
}

/// Read a `profile_setting` row, logging a query failure rather than
/// swallowing it.
///
/// `None` covers both "no such row" and "the read failed": the callers
/// all resolve to their setting's default either way, deliberately. A
/// profile whose database cannot be read is better left on the neutral
/// defaults than on the settings of the profile before it — leaving the
/// atomic untouched is exactly the leak #698 is about. The warning is
/// what makes the difference visible in a diagnostics log.
pub(super) async fn read_setting(pool: &sqlx::SqlitePool, key: &str) -> Option<String> {
    match sqlx::query_scalar::<_, String>("SELECT value FROM profile_setting WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
    {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(key, %err, "reading a profile setting failed, using its default");
            None
        }
    }
}

/// The markers the `value_type` CHECK of `profile_setting` and
/// `app_setting` accepts — and the only ones [`persist_setting`] can
/// write, so a marker the table refuses no longer compiles.
#[derive(Clone, Copy, Debug)]
pub(super) enum SettingType {
    Bool,
    Int,
    /// Anything else, decimals included: the table has no marker for
    /// them, and every reader parses the text back.
    Text,
    Json,
}

impl SettingType {
    pub(super) fn as_marker(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Text => "string",
            Self::Json => "json",
        }
    }
}

/// Upsert one `profile_setting` row, and report a failure to the caller.
///
/// The commands call it **before** they hand the value to the engine, and
/// return its error: a value the database refuses then never reaches the
/// engine, and the control that asked rolls back to what both still hold.
/// It used to run after, with its error dropped — the pre-amp, the fallback
/// gain and the playback speed were written with a marker the table's
/// CHECK refuses, every write failed, and the switch, the engine and a
/// page reset all reported a success the next launch undid.
pub(super) async fn persist_setting(
    pool: &sqlx::SqlitePool,
    key: &str,
    value: impl AsRef<str>,
    value_type: SettingType,
) -> AppResult<()> {
    let result = sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            value_type = excluded.value_type,
            updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value.as_ref())
    .bind(value_type.as_marker())
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(pool)
    .await;
    if let Err(err) = &result {
        tracing::warn!(key, %err, "profile setting not saved");
    }
    result?;
    Ok(())
}

/// Read a boolean `profile_setting`. `"true"` / `"1"` read as true and
/// `"false"` / `"0"` as false; a row that is missing, unreadable or
/// holds anything else resolves to `default`.
///
/// Exists so [`restore_profile_audio_settings`] can store every toggle
/// unconditionally: it runs again on a profile *switch*, and a setting
/// only written when its row exists keeps the previous profile's value
/// on a profile that never touched it (#698).
pub(super) async fn read_bool_setting(pool: &sqlx::SqlitePool, key: &str, default: bool) -> bool {
    read_setting(pool, key)
        .await
        .and_then(|v| match v.as_str() {
            // `"1"` / `"0"` are the older spelling, still on disk in
            // libraries that predate the current writers and still
            // accepted everywhere else that reads a boolean setting
            // (`plugins/`, `canvas.rs`, `profile_io/`, …). Reading
            // `"1"` as false would silently turn a setting off.
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            // Anything else is not an answer: keep the default rather
            // than read a typo as "off".
            _ => None,
        })
        .unwrap_or(default)
}

/// Restore the active profile's audio settings into the engine's
/// process-wide atomics.
///
/// Every setting resolves to the engine's own boot default when its
/// row is missing or unparseable, and is stored either way. That is
/// the whole point: this runs again on a profile *switch*, and a
/// value only written when its row exists keeps the previous
/// profile's setting on a profile that never chose one (#698).
///
/// Called from [`switch_profile`] as the pool is swapped — rather
/// than left to whichever `player_get_state` the frontend makes
/// next — because every getter reads these atomics and nothing
/// orders those reads against that call.
pub(crate) async fn restore_profile_audio_settings(pool: &sqlx::SqlitePool, engine: &AudioEngine) {
    // Volume first, and unconditionally: a profile that never moved the
    // knob is at full volume, not at the level the profile before it
    // chose. `1.0` is what `SharedPlayback::new` starts from. Read here
    // rather than through a helper of its own so a failed query is
    // logged like every other setting in this function; the row is an
    // int 0-100, written by `player_set_volume` below and by MPD.
    let volume = read_setting(pool, "player.volume")
        .await
        .and_then(|v| v.parse::<i64>().ok())
        .map(|v| (v.clamp(0, 100) as f32) / 100.0)
        .unwrap_or(1.0);
    engine.shared().set_volume(volume);
    // Restore audio settings (normalize, mono, crossfade). Each
    // one resolves to its boot default when the row is missing
    // or unparseable and is stored either way — see the block
    // comment on `audio.dsd_dop` below for why a conditional
    // store leaks across a profile switch (#698).
    {
        let normalize = read_bool_setting(pool, "audio.normalize", false).await;
        engine
            .shared()
            .normalize_enabled
            .store(normalize, std::sync::atomic::Ordering::Release);

        let mono = read_bool_setting(pool, "audio.mono", false).await;
        engine
            .shared()
            .mono_enabled
            .store(mono, std::sync::atomic::Ordering::Release);

        // Balance and width: the centre and 100 % when the row is
        // missing or unparseable, clamped by the setters either way.
        let balance = read_setting(pool, "audio.balance")
            .await
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        engine.shared().stereo.set_balance(balance);
        let width = read_setting(pool, "audio.stereo_width")
            .await
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(i32::from(crate::audio::stereo::WIDTH_NEUTRAL));
        engine.shared().stereo.set_width(width);

        let crossfade_ms = read_setting(pool, "audio.crossfade_ms")
            .await
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        engine
            .shared()
            .crossfade_ms
            .store(crossfade_ms, std::sync::atomic::Ordering::Release);
    }
    // DSD converter precision. `player_get_state` re-runs on
    // every hydration (incl. after a profile switch), so resolve
    // to a definite value every time: a missing / corrupt / out-
    // of-set row must reset the atomic to 256 rather than leak
    // the previous profile's choice.
    let dsd_taps = read_setting(pool, "audio.dsd_precision")
        .await
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|t| DSD_TAPS_ALLOWED.contains(t))
        .unwrap_or(256);
    engine
        .shared()
        .dsd_taps
        .store(dsd_taps, std::sync::atomic::Ordering::Release);
    // Native DSD via DoP defaults OFF (#495) — only override the
    // boot-time default when an explicit `true` row is found, and
    // reset to false otherwise so a profile switch can't leak the
    // previous profile's opt-in.
    {
        let dop = read_bool_setting(pool, "audio.dsd_dop", false).await;
        engine
            .shared()
            .dsd_dop_enabled
            .store(dop, std::sync::atomic::Ordering::Release);
    }
    // Per-track rate matching (#600), same shape and same
    // reasoning as `dsd_dop` above: stored either way, so a
    // profile switch cannot leak the previous profile's opt-in.
    {
        let match_rate = read_bool_setting(pool, "audio.match_source_rate", false).await;
        engine
            .shared()
            .match_source_rate
            .store(match_rate, std::sync::atomic::Ordering::Release);
    }
    // Pause on device loss (#617). It lives on the engine rather
    // than in `SharedPlayback`, but it is profile-scoped like the
    // two above, and this block is the one that runs on a profile
    // *switch* — the boot read in `lib.rs` only covers the first
    // profile of the session, so without this the second profile
    // kept the first one's answer. Absent means on.
    {
        let pause_on_loss = read_bool_setting(pool, "audio.pause_on_device_loss", true).await;
        engine.set_pause_on_device_loss(pause_on_loss);
    }
    // Keep the machine awake while playing (`power.rs`). Profile-scoped
    // and read here for the same reason as the setting above. Absent
    // means on.
    crate::power::set_enabled(read_bool_setting(pool, "playback.keep_awake", true).await);
    // Every ReplayGain setting resolves to its default when the
    // row is missing or unparseable, and is stored either way —
    // the `dsd_dop` pattern above, for the same reason: this
    // block also runs on a profile *switch*, so a value left
    // conditional would keep the previous profile's setting on
    // a profile that never set one.
    {
        let shared = engine.shared();
        let enabled = read_bool_setting(pool, "audio.replaygain", false).await;
        shared
            .replaygain_enabled
            .store(enabled, std::sync::atomic::Ordering::Release);

        // The two dB knobs default to 0.0 and are clamped on the
        // way in for the same reason playback speed is: a
        // hand-edited row must not reach the mixer intact.
        for (key, target) in [
            ("audio.replaygain_preamp", &shared.replaygain_preamp_db_bits),
            (
                "audio.replaygain_fallback",
                &shared.replaygain_fallback_db_bits,
            ),
        ] {
            let value = read_setting(pool, key)
                .await
                .and_then(|v| v.parse::<f32>().ok())
                .map(clamp_replaygain_adjust)
                .unwrap_or(0.0);
            target.store(value.to_bits(), std::sync::atomic::Ordering::Release);
        }

        // Clipping prevention defaults to ON, so only an
        // explicit `false` row turns it off.
        let prevent_clipping =
            read_bool_setting(pool, "audio.replaygain_prevent_clipping", true).await;
        shared
            .replaygain_prevent_clipping
            .store(prevent_clipping, std::sync::atomic::Ordering::Release);

        // Which of a file's two gains to apply. Defaults to
        // `Auto`, and an unparseable row reads as the default
        // rather than as an error — same rule as the knobs
        // above, and the reason this is stored unconditionally.
        let mode = read_setting(pool, "audio.replaygain_mode")
            .await
            .map(|v| GainMode::from_setting(&v))
            .unwrap_or_default();
        shared
            .replaygain_mode_bits
            .store(mode.as_bits(), std::sync::atomic::Ordering::Release);
    }
    // Gapless defaults to ON, so an absent row reads as `true`.
    let gapless = read_bool_setting(pool, "audio.gapless", true).await;
    engine
        .shared()
        .gapless_enabled
        .store(gapless, std::sync::atomic::Ordering::Release);
    // Playback speed defaults to 1.0. Out-of-range values are
    // re-clamped on the way in so a hand-edited DB can't
    // resurrect the rubato instability range, and an absent or
    // unparseable row resolves to 1.0 rather than leaving the
    // previous profile's speed in place.
    {
        let speed = read_setting(pool, "audio.playback_speed")
            .await
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|s| s.is_finite())
            .map(|s| s.clamp(0.5, 2.0))
            .unwrap_or(1.0);
        // Use the raw atomic store here instead of
        // `set_playback_speed` — the latter would also
        // rebase `samples_played` / `base_offset_ms`
        // against a position we haven't loaded yet,
        // moving the resume point off the persisted
        // value. `speed_dirty` stays false because no
        // stream is decoding yet; the first track's
        // lazy resampler init picks up the speed via
        // `stream.playback_speed = shared.playback_speed()`.
        engine
            .shared()
            .playback_speed_bits
            .store(speed.to_bits(), std::sync::atomic::Ordering::Release);
    }
    // Equalizer settings. Both halves are restored
    // unconditionally: a profile with no curve of its own must
    // hear a flat one, not the curve the previous profile left
    // in the filters.
    {
        let eq_enabled = read_bool_setting(pool, "audio.eq_enabled", false).await;
        engine.shared().eq.set_enabled(eq_enabled);

        // `set_all_bands_db` only writes the bands the slice
        // covers, so a short (or missing, or malformed) row is
        // padded to a full flat curve first — otherwise the
        // bands it doesn't reach keep their previous values.
        let mut bands = [0.0_f32; crate::audio::eq::BAND_COUNT];
        if let Some(stored) = read_setting(pool, "audio.eq_bands")
            .await
            .and_then(|v| serde_json::from_str::<Vec<f32>>(&v).ok())
        {
            for (slot, g) in bands.iter_mut().zip(stored) {
                *slot = g;
            }
        }
        engine.shared().eq.set_all_bands_db(&bands);
    }
    // Both crossfade refinements default OFF.
    let smart_crossfade = read_bool_setting(pool, "audio.smart_crossfade", false).await;
    engine
        .shared()
        .smart_crossfade_enabled
        .store(smart_crossfade, std::sync::atomic::Ordering::Release);
    let dynamic_crossfade = read_bool_setting(pool, "audio.dynamic_crossfade", false).await;
    engine
        .shared()
        .dynamic_crossfade_enabled
        .store(dynamic_crossfade, std::sync::atomic::Ordering::Release);
    // Visualizer toggle. Default ON so the spectrum is found without a
    // trip to Settings. The analysis runs on the decoder thread, up to ~30
    // FFTs and spectrum events a second (`EMIT_INTERVAL` in
    // `audio/spectrum.rs`); turning it off short-circuits it entirely.
    let visualizer = read_bool_setting(pool, "ui.visualizer", true).await;
    engine
        .shared()
        .visualizer_enabled
        .store(visualizer, std::sync::atomic::Ordering::Release);
}
