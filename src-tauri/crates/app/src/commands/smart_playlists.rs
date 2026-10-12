//! Frontend-facing entry points for the smart-playlist engine.
//!
//! Kept thin on purpose — the heavy lifting lives in
//! [`crate::smart_playlists::generator`] so it stays unit-testable without a
//! Tauri runtime.

use crate::error::{AppError, AppResult};
use crate::smart_playlists::{
    custom::{self, CustomRules},
    generator, on_repeat,
    themed::{self, MixLabels},
    PathsContext, SmartPlaylistRules,
};
use crate::state::AppState;

/// Build the path bundle the smart-playlist generators expect from
/// the desktop's `AppPaths`. The clones are cheap (`PathBuf` is just
/// a `Vec<u8>` under the hood) and centralising the construction
/// here keeps every regen call site one line shorter.
fn paths_ctx(state: &AppState, profile_id: i64) -> PathsContext {
    PathsContext {
        metadata_artwork_dir: state.paths.metadata_artwork_dir.clone(),
        app_db_path: state.paths.app_db.clone(),
        profile_artwork_dir: state.paths.profile_artwork_dir(profile_id),
    }
}
use serde::Deserialize;
use tauri::{AppHandle, Emitter};

/// Regenerate every Daily Mix slot from the active profile's listening
/// history. Returns the playlist ids in slot order so the frontend can
/// optimistically refresh and navigate to the first one.
#[tauri::command]
pub async fn regenerate_daily_mixes(state: tauri::State<'_, AppState>) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let ctx = paths_ctx(&state, profile_id);
    Ok(generator::regenerate_daily_mixes(&pool, &ctx, profile_id).await?)
}

/// Regenerate the active profile's On Repeat playlist from the last
/// 30 days of listening history. Returns the playlist id, or `null`
/// when there's not enough data to materialize anything (the previous
/// row, if any, is cleaned up in that case).
#[tauri::command]
pub async fn regenerate_on_repeat(state: tauri::State<'_, AppState>) -> AppResult<Option<i64>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let ctx = paths_ctx(&state, profile_id);
    Ok(on_repeat::regenerate_on_repeat(&pool, &ctx).await?)
}

/// One-shot regen for every built-in smart-playlist family (Daily Mix
/// slots, On Repeat, the genre and decade mixes). The frontend's
/// "Regenerate" button calls this so the user gets the whole "Made for
/// you" surface refreshed in a single click without having to know about
/// the family split.
#[derive(Debug, serde::Serialize)]
pub struct RegenerateAllSmartPlaylistsOutput {
    pub daily_mix_ids: Vec<i64>,
    pub on_repeat_id: Option<i64>,
    pub genre_mix_ids: Vec<i64>,
    pub decade_mix_ids: Vec<i64>,
}

/// `labels` carries the words around the genre and decade mixes' names,
/// in the user's language: the backend knows the genre and the decade,
/// not the language the interface speaks.
#[tauri::command]
pub async fn regenerate_all_smart_playlists(
    state: tauri::State<'_, AppState>,
    labels: MixLabels,
) -> AppResult<RegenerateAllSmartPlaylistsOutput> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let ctx = paths_ctx(&state, profile_id);
    let daily_mix_ids = generator::regenerate_daily_mixes(&pool, &ctx, profile_id).await?;
    let on_repeat_id = on_repeat::regenerate_on_repeat(&pool, &ctx).await?;
    let genre_mix_ids = themed::regenerate_genre_mixes(&pool, &ctx, profile_id, &labels).await?;
    let decade_mix_ids = themed::regenerate_decade_mixes(&pool, &ctx, profile_id, &labels).await?;
    Ok(RegenerateAllSmartPlaylistsOutput {
        daily_mix_ids,
        on_repeat_id,
        genre_mix_ids,
        decade_mix_ids,
    })
}

#[derive(Debug, Deserialize)]
pub struct CustomSmartPlaylistInput {
    pub name: String,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
    pub rules: CustomRules,
}

/// Create a new custom smart playlist + materialize its tracks. The
/// playlist row is persisted with `is_smart = 1` and the rule set
/// stored in `playlist.smart_rules` so [`regenerate_custom_smart_playlist`]
/// can re-run it later. Returns the new playlist id and the count of
/// tracks materialized.
#[derive(Debug, serde::Serialize)]
pub struct CustomSmartPlaylistOutput {
    pub playlist_id: i64,
    pub track_count: i64,
}

#[tauri::command]
pub async fn create_custom_smart_playlist(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    input: CustomSmartPlaylistInput,
) -> AppResult<CustomSmartPlaylistOutput> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Other("playlist name cannot be empty".into()));
    }
    let pool = state.require_profile_pool().await?;
    let now = chrono::Utc::now().timestamp_millis();
    let color_id = input.color_id.unwrap_or_else(|| "violet".to_string());
    let icon_id = input.icon_id.unwrap_or_else(|| "sparkles".to_string());
    // Serialize before the INSERT so a malformed Custom rules payload
    // surfaces as a clean error instead of an unrecognisable
    // smart_rules blob written to the row.
    let rules_json = SmartPlaylistRules::Custom {
        rules: input.rules.clone(),
    }
    .to_json()
    .map_err(|e| AppError::Other(format!("smart rules serialize: {e}")))?;

    let insert = sqlx::query(
        "INSERT INTO playlist
             (name, description, color_id, icon_id, is_smart, smart_rules,
              position, created_at, updated_at)
         VALUES (?, ?, ?, ?, 1, ?, 0, ?, ?)",
    )
    .bind(&name)
    .bind(input.description.as_deref())
    .bind(&color_id)
    .bind(&icon_id)
    .bind(&rules_json)
    .bind(now)
    .bind(now)
    .execute(&*pool)
    .await?;
    let playlist_id = insert.last_insert_rowid();

    let track_count = custom::materialize(&pool, playlist_id, &input.rules).await?;
    let _ = app.emit("playlist:changed", playlist_id);

    Ok(CustomSmartPlaylistOutput {
        playlist_id,
        track_count,
    })
}

/// Update the rule set of an existing custom smart playlist and
/// re-materialize. The playlist must already be `is_smart = 1` with a
/// `Custom { ... }` rules payload — calling this on a Daily Mix or a
/// manual playlist returns an error.
#[tauri::command]
pub async fn update_custom_smart_playlist(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    input: CustomSmartPlaylistInput,
) -> AppResult<CustomSmartPlaylistOutput> {
    let pool = state.require_profile_pool().await?;
    let row: Option<(i64, Option<String>)> =
        sqlx::query_as("SELECT is_smart, smart_rules FROM playlist WHERE id = ?")
            .bind(playlist_id)
            .fetch_optional(&*pool)
            .await?;
    let (is_smart, existing_rules) =
        row.ok_or_else(|| AppError::Other(format!("playlist {playlist_id} not found")))?;
    if is_smart != 1 {
        return Err(AppError::Other("playlist is not a smart playlist".into()));
    }
    if !is_custom_payload(existing_rules.as_deref()) {
        return Err(AppError::Other(
            "playlist is a generated smart playlist (Daily Mix, On Repeat, a genre or decade mix), not a custom one".into(),
        ));
    }

    let now = chrono::Utc::now().timestamp_millis();
    let rules_json = SmartPlaylistRules::Custom {
        rules: input.rules.clone(),
    }
    .to_json()
    .map_err(|e| AppError::Other(format!("smart rules serialize: {e}")))?;
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Other("playlist name cannot be empty".into()));
    }
    let color_id = input.color_id.unwrap_or_else(|| "violet".to_string());
    let icon_id = input.icon_id.unwrap_or_else(|| "sparkles".to_string());

    sqlx::query(
        "UPDATE playlist
            SET name = ?, description = ?, color_id = ?, icon_id = ?,
                smart_rules = ?, updated_at = ?
          WHERE id = ?",
    )
    .bind(&name)
    .bind(input.description.as_deref())
    .bind(&color_id)
    .bind(&icon_id)
    .bind(&rules_json)
    .bind(now)
    .bind(playlist_id)
    .execute(&*pool)
    .await?;

    let track_count = custom::materialize(&pool, playlist_id, &input.rules).await?;
    let _ = app.emit("playlist:changed", playlist_id);

    Ok(CustomSmartPlaylistOutput {
        playlist_id,
        track_count,
    })
}

/// Re-materialize an existing custom smart playlist from its stored
/// rules. Useful after the library changes (new files imported, tracks
/// re-tagged) so the membership picks up the new rows without forcing
/// the user to re-open the rule editor.
#[tauri::command]
pub async fn regenerate_custom_smart_playlist(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<CustomSmartPlaylistOutput> {
    let pool = state.require_profile_pool().await?;
    let row: Option<(i64, Option<String>)> =
        sqlx::query_as("SELECT is_smart, smart_rules FROM playlist WHERE id = ?")
            .bind(playlist_id)
            .fetch_optional(&*pool)
            .await?;
    let (is_smart, raw) =
        row.ok_or_else(|| AppError::Other(format!("playlist {playlist_id} not found")))?;
    if is_smart != 1 {
        return Err(AppError::Other("playlist is not a smart playlist".into()));
    }
    let rules = parse_custom_rules(raw.as_deref())?;
    let track_count = custom::materialize(&pool, playlist_id, &rules).await?;
    let _ = app.emit("playlist:changed", playlist_id);
    Ok(CustomSmartPlaylistOutput {
        playlist_id,
        track_count,
    })
}

/// Read the rule set of a custom smart playlist for the editor's
/// "Edit" path. Errors when the playlist isn't a Custom one.
#[tauri::command]
pub async fn get_custom_smart_playlist_rules(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<CustomRules> {
    let pool = state.require_profile_pool().await?;
    let raw: Option<Option<String>> =
        sqlx::query_scalar("SELECT smart_rules FROM playlist WHERE id = ? AND is_smart = 1")
            .bind(playlist_id)
            .fetch_optional(&*pool)
            .await?;
    let raw = raw
        .flatten()
        .ok_or_else(|| AppError::Other(format!("smart playlist {playlist_id} not found")))?;
    parse_custom_rules(Some(&raw))
}

/// Count what the current rule set matches, without listing it.
///
/// The editor calls this on every edit, so it must stay cheap: no
/// sort, no truncation, no ids crossing the IPC boundary. It also
/// answers the question the preview list cannot — how many tracks the
/// rules match *beyond* the limit, which is what tells the user their
/// "50 most recent" rule is picking from three thousand and not from
/// fifty-one.
#[tauri::command]
pub async fn count_custom_smart_playlist(
    state: tauri::State<'_, AppState>,
    rules: CustomRules,
) -> AppResult<custom::RulesCount> {
    let pool = state.require_profile_pool().await?;
    Ok(custom::count_matches(&pool, &rules).await?)
}

fn is_custom_payload(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    matches!(
        serde_json::from_str::<SmartPlaylistRules>(raw),
        Ok(SmartPlaylistRules::Custom { .. })
    )
}

fn parse_custom_rules(raw: Option<&str>) -> AppResult<CustomRules> {
    let raw = raw.ok_or_else(|| AppError::Other("smart_rules column is empty".into()))?;
    match serde_json::from_str::<SmartPlaylistRules>(raw) {
        Ok(SmartPlaylistRules::Custom { rules }) => Ok(rules),
        Ok(_) => Err(AppError::Other(
            "playlist is a generated smart playlist (Daily Mix, On Repeat, a genre or decade mix), not a custom one".into(),
        )),
        Err(e) => Err(AppError::Other(format!("invalid smart_rules JSON: {e}"))),
    }
}

#[cfg(test)]
mod themed_tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;
    use std::str::FromStr;

    /// The real migrator, `foreign_keys` on: the generators write
    /// `playlist` and `playlist_track` rows the schema has to accept.
    async fn pool() -> SqlitePool {
        let options = waveflow_core::repository::sqlite::collation::register(
            SqliteConnectOptions::from_str(":memory:")
                .unwrap()
                .foreign_keys(true),
        );
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        pool
    }

    fn labels() -> MixLabels {
        MixLabels {
            genre_name: "{name} Mix".into(),
            genre_description: "The best of {name}".into(),
            decade_name: "{decade}s".into(),
            decade_description: "Your {decade}s".into(),
        }
    }

    /// Rock: 30 tracks by 5 artists, all from the 1990s, played. Jazz: 10
    /// tracks by 2 artists, too small for a mix, all from the 1970s.
    async fn seed(pool: &SqlitePool) {
        let mut statements = vec![
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)"
                .to_string(),
            "INSERT INTO genre (id, name, canonical_name) VALUES (1, 'Rock', 'rock'),
                                                                 (2, 'Jazz', 'jazz')"
                .to_string(),
        ];
        for a in 1..=7 {
            statements.push(format!(
                "INSERT INTO artist (id, name, canonical_name) VALUES ({a}, 'A{a}', 'a{a}')"
            ));
        }
        let mut id = 0;
        let mut track = |genre: i64, artist: i64, year: i64, statements: &mut Vec<String>| {
            id += 1;
            statements.push(format!(
                "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                    title, year, duration_ms, added_at, is_available,
                                    hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical,
                                    primary_artist)
                 VALUES ({id}, 1, '/m/{id}.flac', 'h{id}', 1, 0, 'T{id}', {year}, 1000, 0, 1,
                         0, 0, 0, 0, {artist})"
            ));
            statements.push(format!(
                "INSERT INTO track_genre (track_id, genre_id) VALUES ({id}, {genre})"
            ));
        };
        for n in 0..30 {
            track(1, 1 + n % 5, 1990 + n % 10, &mut statements);
        }
        for n in 0..10 {
            track(2, 6 + n % 2, 1975, &mut statements);
        }
        let now = chrono::Utc::now().timestamp_millis();
        statements.push(format!(
            "INSERT INTO play_event (track_id, played_at, listened_ms, completed, skipped)
             VALUES (1, {now}, 60000, 1, 0)"
        ));
        for statement in statements {
            sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
                .execute(pool)
                .await
                .unwrap();
        }
    }

    fn ctx(dir: &std::path::Path) -> PathsContext {
        PathsContext {
            metadata_artwork_dir: dir.join("metadata"),
            app_db_path: dir.join("app.db"),
            profile_artwork_dir: dir.join("artwork"),
        }
    }

    async fn names(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM playlist WHERE is_smart = 1 ORDER BY position")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_genre_and_a_decade_big_enough_get_a_mix_named_in_the_labels() {
        let pool = pool().await;
        seed(&pool).await;
        let dir = tempfile::tempdir().unwrap();
        let genres = themed::regenerate_genre_mixes(&pool, &ctx(dir.path()), 1, &labels())
            .await
            .unwrap();
        let decades = themed::regenerate_decade_mixes(&pool, &ctx(dir.path()), 1, &labels())
            .await
            .unwrap();
        // Jazz and the 1970s hold ten tracks by two artists: no mix.
        assert_eq!((genres.len(), decades.len()), (1, 1));
        assert_eq!(names(&pool).await, ["Rock Mix", "1990s"]);

        // At most three tracks per artist, every one of them Rock.
        let per_artist: Vec<i64> = sqlx::query_scalar(
            "SELECT COUNT(*) FROM playlist_track pt JOIN track t ON t.id = pt.track_id
              WHERE pt.playlist_id = ? GROUP BY t.primary_artist",
        )
        .bind(genres[0])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(per_artist.len(), 5);
        assert!(per_artist.iter().all(|n| *n <= 3));
    }

    #[tokio::test]
    async fn a_seed_that_no_longer_qualifies_takes_its_mix_with_it() {
        let pool = pool().await;
        seed(&pool).await;
        let dir = tempfile::tempdir().unwrap();
        let first = themed::regenerate_genre_mixes(&pool, &ctx(dir.path()), 1, &labels())
            .await
            .unwrap();
        assert_eq!(first.len(), 1);
        // A regen that rewrites the same mix keeps its row.
        let again = themed::regenerate_genre_mixes(&pool, &ctx(dir.path()), 1, &labels())
            .await
            .unwrap();
        assert_eq!(again, first);

        sqlx::query("UPDATE track SET is_available = 0 WHERE id > 20")
            .execute(&pool)
            .await
            .unwrap();
        let after = themed::regenerate_genre_mixes(&pool, &ctx(dir.path()), 1, &labels())
            .await
            .unwrap();
        assert!(after.is_empty());
        assert!(names(&pool).await.is_empty());
    }
}
