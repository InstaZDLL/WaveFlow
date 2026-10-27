//! The lyrics preferences: the translation language, Prefer LRCLIB,
//! and the default destination of edited lyrics.

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

/// `profile_setting` key for the user-picked Musixmatch translation
/// target language. Empty string / absent row → no translation
/// (default). ISO 639-1 lowercase code otherwise (`"en"`, `"fr"`, …).
pub const TRANSLATION_LANG_KEY: &str = "lyrics.translation_lang";

/// Read the per-profile translation language. Returns `None` when
/// the row is absent OR when the value is blank — both surface as
/// "no translation" to the rest of the waterfall.
pub(super) async fn read_translation_lang(pool: &sqlx::SqlitePool) -> AppResult<Option<String>> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(TRANSLATION_LANG_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(value.filter(|v| !v.trim().is_empty()))
}

/// `profile_setting` key: prefer the online providers (LRCLIB / Musixmatch
/// / fallback chain) over the track's own embedded + sidecar lyrics.
/// Default off — the local tiers win, as they historically have.
pub const PREFER_LRCLIB_KEY: &str = "lyrics.prefer_lrclib";

/// Read the per-profile prefer-LRCLIB flag. A missing row / blank / any
/// non-truthy value → `false` (local-first, the historical default). A
/// SQL error propagates rather than masquerading as "disabled" so the
/// caller (and the Settings toggle) sees a real read failure.
pub(super) async fn read_prefer_lrclib(pool: &sqlx::SqlitePool) -> AppResult<bool> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(PREFER_LRCLIB_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(value.map(|v| v == "true" || v == "1").unwrap_or(false))
}

/// Whitelist of language codes the Musixmatch translation endpoint
/// accepts. Mirrors the union of locale codes WaveFlow ships UI for
/// (CLAUDE.md "Language" section) — the 17 i18n locales — minus the
/// `pt-BR` regional variant since Musixmatch only knows the base
/// `pt` code, and minus `kr` (legacy alias for `ko`). Any value
/// outside this set is rejected with a clear error so a typoed
/// IPC payload can't land a row that silently disables translation
/// across every fetch.
const SUPPORTED_TRANSLATION_LANGS: &[&str] = &[
    "en", "es", "de", "it", "nl", "pt", "ru", "tr", "id", "ja", "ko", "zh-CN", "zh-TW", "ar", "hi",
    "fr",
];

/// Read the per-profile translation language flag the lyrics UI
/// pre-fills its dropdown with. Empty / absent → no translation.
#[tauri::command]
pub async fn get_lyrics_translation_lang(
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<String>> {
    let pool = state.require_profile_pool().await?;
    read_translation_lang(&pool).await
}

/// Persist the per-profile translation language flag. Passing
/// `None` clears the row → translation disabled. The whitelist
/// guards against an unknown / typoed code silently breaking the
/// fetch (Musixmatch would return an empty body, which our merge
/// pass would treat as "nothing to inject" — wasted hop).
#[tauri::command]
pub async fn set_lyrics_translation_lang(
    state: tauri::State<'_, AppState>,
    lang: Option<String>,
) -> AppResult<Option<String>> {
    let pool = state.require_profile_pool().await?;
    let normalized: Option<String> = match lang.as_deref() {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else if !SUPPORTED_TRANSLATION_LANGS.contains(&trimmed) {
                return Err(AppError::Other(format!(
                    "set_lyrics_translation_lang: unsupported language '{trimmed}'",
                )));
            } else {
                Some(trimmed.to_string())
            }
        }
        None => None,
    };
    match &normalized {
        Some(code) => {
            let now = chrono::Utc::now().timestamp_millis();
            sqlx::query(
                "INSERT INTO profile_setting (key, value, value_type, updated_at)
                 VALUES (?, ?, 'string', ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, value_type = excluded.value_type, updated_at = excluded.updated_at",
            )
            .bind(TRANSLATION_LANG_KEY)
            .bind(code)
            .bind(now)
            .execute(&*pool)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM profile_setting WHERE key = ?")
                .bind(TRANSLATION_LANG_KEY)
                .execute(&*pool)
                .await?;
        }
    }
    Ok(normalized)
}

/// Read the per-profile prefer-LRCLIB flag for the Settings toggle.
/// Absent / blank → `false` (local lyrics win, the historical default).
#[tauri::command]
pub async fn get_prefer_lrclib(state: tauri::State<'_, AppState>) -> AppResult<bool> {
    let pool = state.require_profile_pool().await?;
    read_prefer_lrclib(&pool).await
}

/// Persist the per-profile prefer-LRCLIB flag. `true` makes the on-demand
/// lyrics waterfall try the online providers before the track's own
/// embedded + sidecar lyrics (issue #378); `false` clears the row and
/// restores the local-first default.
#[tauri::command]
pub async fn set_prefer_lrclib(state: tauri::State<'_, AppState>, enabled: bool) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    if enabled {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES (?, 'true', 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, value_type = excluded.value_type, updated_at = excluded.updated_at",
        )
        .bind(PREFER_LRCLIB_KEY)
        .bind(now)
        .execute(&*pool)
        .await?;
    } else {
        sqlx::query("DELETE FROM profile_setting WHERE key = ?")
            .bind(PREFER_LRCLIB_KEY)
            .execute(&*pool)
            .await?;
    }
    Ok(())
}

/// `app_setting` key holding the global default lyrics destination
/// the editor pre-fills the segmented control with. App-wide rather
/// than per-profile because the destination drives filesystem state
/// (tag bytes, sidecar files) that is shared between profiles whose
/// libraries touch the same audio files — a per-profile choice
/// would create a false sense of isolation.
pub const LYRICS_DEFAULT_DESTINATION_KEY: &str = "lyrics.default_destination";

/// Read the app-wide destination through a **profile** pool, which has
/// `app.db` attached as `app` — so the fetch paths, which hold a profile
/// pool and no `AppState`, can ask without threading one through.
///
/// Missing or unparseable row falls back to `Tag`, like the command below.
pub(super) async fn read_default_destination(
    pool: &sqlx::SqlitePool,
) -> AppResult<LyricsDestination> {
    let row: Option<String> = sqlx::query_scalar("SELECT value FROM app.app_setting WHERE key = ?")
        .bind(LYRICS_DEFAULT_DESTINATION_KEY)
        .fetch_optional(pool)
        .await?;
    Ok(row
        .as_deref()
        .and_then(LyricsDestination::parse)
        .unwrap_or(LyricsDestination::Tag))
}

/// Read the app-wide default. Missing row falls back to `Tag` so
/// existing installs keep the pre-#201 behaviour until the user opts
/// into the new flow (either from the onboarding step or the
/// Settings → Lyrics card).
#[tauri::command]
pub async fn get_lyrics_default_destination(
    state: tauri::State<'_, AppState>,
) -> AppResult<String> {
    let row: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(LYRICS_DEFAULT_DESTINATION_KEY)
        .fetch_optional(&state.app_db)
        .await?;
    let resolved = row
        .as_deref()
        .and_then(LyricsDestination::parse)
        .unwrap_or(LyricsDestination::Tag);
    Ok(resolved.as_str().to_string())
}

/// Persist the user's pick. Rejects unknown values with an explicit
/// error so a typo from the frontend doesn't silently land an
/// unparseable row that the next read would discard back to default.
#[tauri::command]
pub async fn set_lyrics_default_destination(
    state: tauri::State<'_, AppState>,
    destination: String,
) -> AppResult<String> {
    let parsed = LyricsDestination::parse(&destination).ok_or_else(|| {
        AppError::Other(format!(
            "set_lyrics_default_destination: unsupported value '{destination}' (expected tag, sidecar, db_only)"
        ))
    })?;
    let value = parsed.as_str();
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            value_type = excluded.value_type,
            updated_at = excluded.updated_at",
    )
    .bind(LYRICS_DEFAULT_DESTINATION_KEY)
    .bind(value)
    .bind(now)
    .execute(&state.app_db)
    .await?;
    Ok(value.to_string())
}
