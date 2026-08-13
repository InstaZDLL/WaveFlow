//! Writing the server's user data into the local projection.
//!
//! Pure database work: no HTTP, no client, nothing that needs a network
//! to test. The orchestrator in [`super::sync`] decides *when* to call
//! this and fetches whatever metadata is missing afterwards.
//!
//! ## The rule that governs every apply: an absent key means unchanged
//!
//! Journal payloads are not uniform, and the difference is not cosmetic.
//! Creating a playlist emits
//!
//! ```json
//! {"id": "…", "name": "Mix", "track_ids": ["…"]}
//! ```
//!
//! while updating one emits `name`, `comment`, `public` and `track_ids`.
//! A share update omits `track_ids` entirely, because updating a share
//! cannot change them.
//!
//! So decoding a payload into a struct of `Option` fields and writing
//! all of them would blank a playlist's comment on every rename and
//! empty a share on every description edit. Each field is written only
//! when its key is **present**; a key present and null is a genuine
//! clear, and the two are held apart everywhere below.
//!
//! ## Failure has two meanings
//!
//! [`Outcome::Ignored`] is an event this build does not understand — an
//! unknown entity type or action. The protocol says to skip it and keep
//! the cursor moving; a newer server talking to an older client is
//! expected, not broken.
//!
//! An `Err` is different: a *known* event that could not be applied. The
//! caller's contract is then to discard the projection and take a fresh
//! snapshot, because the local state can no longer be trusted to reflect
//! the journal.

use serde_json::Value;
use sqlx::SqliteConnection;

use crate::{
    error::AppResult,
    remote::dto::{SongItem, SyncChange, SyncSnapshot},
};

/// What an apply did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    /// Unknown entity type or action. Skipped on purpose — see the
    /// module docs.
    Ignored,
}

/// Ratings are 1..=5 in storage; `0` on the wire means "clear".
///
/// Clamping rather than rejecting is deliberate: a value outside the
/// range would otherwise abort the transaction, and per the protocol
/// that forces a fresh snapshot — whose replay would abort on the same
/// row, looping forever. Clamping degrades one field instead.
fn clamp_rating(rating: i64) -> i64 {
    rating.clamp(1, 5)
}

/// Replace the whole projection with one writer-consistent snapshot.
///
/// The caller must run this inside a transaction: a projection emptied
/// but not refilled is a state no reader can interpret, and the whole
/// point of a snapshot is that it lands at once or not at all.
///
/// The pending outbound queue is untouched. It holds writes the server
/// has not seen, so it is the one thing here that is not
/// reconstructible.
///
/// And for the same reason, a playlist still carrying a `local:`
/// placeholder **survives** the replacement. It exists only here, the
/// server has never heard of it, so a snapshot cannot possibly be
/// evidence that it was deleted — wiping it would silently destroy a
/// playlist the user created offline while its creation sat in the
/// queue.
pub async fn apply_snapshot(conn: &mut SqliteConnection, snapshot: &SyncSnapshot) -> AppResult<()> {
    let placeholder_pattern = format!("{}%", crate::remote::mutation::PLACEHOLDER_PREFIX);
    for statement in [
        "DELETE FROM remote_playlist_track WHERE playlist_remote_id NOT LIKE ?",
        "DELETE FROM remote_playlist WHERE remote_id NOT LIKE ?",
        "DELETE FROM remote_share_track WHERE share_remote_id NOT LIKE ?",
        "DELETE FROM remote_share WHERE remote_id NOT LIKE ?",
    ] {
        sqlx::query(statement)
            .bind(&placeholder_pattern)
            .execute(&mut *conn)
            .await?;
    }
    for statement in [
        "DELETE FROM remote_favorite",
        "DELETE FROM remote_rating",
        "DELETE FROM remote_history",
        "DELETE FROM remote_queue_track",
        "DELETE FROM remote_queue",
    ] {
        sqlx::query(statement).execute(&mut *conn).await?;
    }

    for playlist in &snapshot.playlists {
        sqlx::query(
            "INSERT INTO remote_playlist
                (remote_id, name, comment, is_public, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&playlist.id)
        .bind(&playlist.name)
        .bind(playlist.comment.as_deref())
        .bind(i64::from(playlist.public))
        .bind(playlist.created_at)
        .bind(playlist.updated_at)
        .execute(&mut *conn)
        .await?;

        for (position, song) in playlist.songs.iter().enumerate() {
            cache_song(&mut *conn, song).await?;
            sqlx::query(
                "INSERT INTO remote_playlist_track
                    (playlist_remote_id, position, track_remote_id)
                 VALUES (?, ?, ?)",
            )
            .bind(&playlist.id)
            .bind(position as i64)
            .bind(&song.id)
            .execute(&mut *conn)
            .await?;
        }
    }

    for favorite in &snapshot.favorites {
        sqlx::query(
            "INSERT INTO remote_favorite (entity_type, entity_id, starred_at)
             VALUES (?, ?, ?)
             ON CONFLICT(entity_type, entity_id) DO UPDATE
                SET starred_at = excluded.starred_at",
        )
        .bind(&favorite.entity_type)
        .bind(&favorite.entity_id)
        .bind(favorite.starred_at)
        .execute(&mut *conn)
        .await?;
    }

    for rating in &snapshot.ratings {
        // A stored zero would mean "rated nothing", which is not a
        // rating. The wire uses it as an instruction, so it never
        // reaches storage.
        if rating.rating <= 0 {
            continue;
        }
        sqlx::query(
            "INSERT INTO remote_rating (entity_type, entity_id, rating, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(entity_type, entity_id) DO UPDATE
                SET rating = excluded.rating, updated_at = excluded.updated_at",
        )
        .bind(&rating.entity_type)
        .bind(&rating.entity_id)
        .bind(clamp_rating(rating.rating))
        .bind(rating.updated_at)
        .execute(&mut *conn)
        .await?;
    }

    for play in &snapshot.history {
        // `INSERT OR REPLACE`: a snapshot may legitimately repeat a
        // play we already hold, and the natural key is all we have to
        // recognize it by.
        sqlx::query(
            "INSERT OR REPLACE INTO remote_history (track_remote_id, played_at, submission)
             VALUES (?, ?, ?)",
        )
        .bind(&play.track_id)
        .bind(play.played_at)
        .bind(i64::from(play.submission))
        .execute(&mut *conn)
        .await?;
    }

    if let Some(queue) = &snapshot.queue {
        sqlx::query(
            "INSERT INTO remote_queue (id, current_remote_id, position_ms, changed_by, updated_at)
             VALUES (1, ?, ?, ?, ?)",
        )
        .bind(queue.current.as_deref())
        .bind(queue.position_ms.max(0))
        .bind(queue.changed_by.as_deref())
        .bind(queue.updated_at)
        .execute(&mut *conn)
        .await?;

        for (position, song) in queue.songs.iter().enumerate() {
            cache_song(&mut *conn, song).await?;
            sqlx::query("INSERT INTO remote_queue_track (position, track_remote_id) VALUES (?, ?)")
                .bind(position as i64)
                .bind(&song.id)
                .execute(&mut *conn)
                .await?;
        }
    }

    for share in &snapshot.shares {
        sqlx::query(
            "INSERT INTO remote_share
                (remote_id, description, expires_at, visit_count, url, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&share.id)
        .bind(share.description.as_deref())
        .bind(share.expires_at)
        .bind(share.visit_count.max(0))
        .bind(share.url.as_deref())
        .bind(share.created_at)
        .execute(&mut *conn)
        .await?;

        replace_share_tracks(&mut *conn, &share.id, &share.track_ids).await?;
    }

    Ok(())
}

/// Store what we know about a track, without losing what we already had.
///
/// `COALESCE(excluded.…, …)` on every optional column: a later sighting
/// of the same track from a sparser source must not blank fields an
/// earlier, richer one filled in.
pub async fn cache_song(conn: &mut SqliteConnection, song: &SongItem) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO remote_track
            (remote_id, title, artist, album, album_id, duration_ms, track_no, disc_no,
             year, genre, suffix, bitrate, size, artwork_hash, library_id, full_hash, cached_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(remote_id) DO UPDATE SET
            title        = excluded.title,
            artist       = COALESCE(excluded.artist, artist),
            album        = COALESCE(excluded.album, album),
            album_id     = COALESCE(excluded.album_id, album_id),
            duration_ms  = MAX(excluded.duration_ms, duration_ms),
            track_no     = COALESCE(excluded.track_no, track_no),
            disc_no      = COALESCE(excluded.disc_no, disc_no),
            year         = COALESCE(excluded.year, year),
            genre        = COALESCE(excluded.genre, genre),
            suffix       = COALESCE(excluded.suffix, suffix),
            bitrate      = COALESCE(excluded.bitrate, bitrate),
            size         = COALESCE(excluded.size, size),
            artwork_hash = COALESCE(excluded.artwork_hash, artwork_hash),
            library_id   = COALESCE(excluded.library_id, library_id),
            full_hash    = COALESCE(excluded.full_hash, full_hash),
            cached_at    = excluded.cached_at",
    )
    .bind(&song.id)
    // An untitled row is still better than no row: the playlist keeps
    // its ordering and the identifier is there to re-fetch with.
    .bind(song.title.as_deref().unwrap_or_default())
    .bind(song.artist.as_deref())
    .bind(song.album.as_deref())
    .bind(song.album_id.as_deref())
    .bind(song.duration_ms.unwrap_or(0).max(0))
    .bind(song.track)
    .bind(song.disc)
    .bind(song.year)
    .bind(song.genre.as_deref())
    .bind(song.suffix.as_deref())
    .bind(song.bitrate)
    .bind(song.size)
    .bind(song.artwork_hash.as_deref())
    .bind(song.library_id.as_deref())
    .bind(song.full_hash.as_deref())
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Track identifiers the projection references but has no metadata for.
///
/// The gap is structural, not a bug: a snapshot carries whole song
/// objects, a change event carries bare identifiers. The orchestrator
/// closes it by fetching these, and until it does the UI has an
/// identifier and an ordering, which is enough to render a placeholder
/// rather than nothing.
pub async fn missing_track_ids(conn: &mut SqliteConnection) -> AppResult<Vec<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT track_remote_id FROM remote_playlist_track
          WHERE track_remote_id NOT IN (SELECT remote_id FROM remote_track)
         UNION
         SELECT track_remote_id FROM remote_queue_track
          WHERE track_remote_id NOT IN (SELECT remote_id FROM remote_track)
         UNION
         SELECT track_remote_id FROM remote_share_track
          WHERE track_remote_id NOT IN (SELECT remote_id FROM remote_track)",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows)
}

/// Apply one journal event.
pub async fn apply_change(conn: &mut SqliteConnection, change: &SyncChange) -> AppResult<Outcome> {
    match (change.entity_type.as_str(), change.action.as_str()) {
        ("playlist", "upsert") => apply_playlist_upsert(conn, change).await,
        ("playlist", "delete") => {
            // The child rows go explicitly: `ON DELETE CASCADE` only
            // fires with `foreign_keys` on, which is a per-connection
            // pragma, and correctness should not depend on how the
            // caller happened to connect.
            sqlx::query("DELETE FROM remote_playlist_track WHERE playlist_remote_id = ?")
                .bind(&change.entity_id)
                .execute(&mut *conn)
                .await?;
            sqlx::query("DELETE FROM remote_playlist WHERE remote_id = ?")
                .bind(&change.entity_id)
                .execute(&mut *conn)
                .await?;
            Ok(Outcome::Applied)
        }

        ("favorite", "upsert") => {
            let entity_type = payload_str(&change.payload, "entity_type")
                .unwrap_or("track")
                .to_string();
            let entity_id = payload_str(&change.payload, "entity_id")
                .unwrap_or(&change.entity_id)
                .to_string();
            // `starred: false` on an upsert is a removal. The server
            // already picks the action from the same boolean, but
            // honouring the payload keeps the two from disagreeing.
            if change.payload.get("starred").and_then(Value::as_bool) == Some(false) {
                delete_favorite(conn, &entity_type, &entity_id).await?;
            } else {
                sqlx::query(
                    "INSERT INTO remote_favorite (entity_type, entity_id, starred_at)
                     VALUES (?, ?, ?)
                     ON CONFLICT(entity_type, entity_id) DO UPDATE
                        SET starred_at = excluded.starred_at",
                )
                .bind(&entity_type)
                .bind(&entity_id)
                .bind(change.changed_at)
                .execute(&mut *conn)
                .await?;
            }
            Ok(Outcome::Applied)
        }
        ("favorite", "delete") => {
            let entity_type = payload_str(&change.payload, "entity_type")
                .unwrap_or("track")
                .to_string();
            let entity_id = payload_str(&change.payload, "entity_id")
                .unwrap_or(&change.entity_id)
                .to_string();
            delete_favorite(conn, &entity_type, &entity_id).await?;
            Ok(Outcome::Applied)
        }

        ("rating", "upsert") | ("rating", "delete") => {
            let entity_type = payload_str(&change.payload, "entity_type")
                .unwrap_or("track")
                .to_string();
            let entity_id = payload_str(&change.payload, "entity_id")
                .unwrap_or(&change.entity_id)
                .to_string();
            let rating = change
                .payload
                .get("rating")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if change.action == "delete" || rating <= 0 {
                sqlx::query("DELETE FROM remote_rating WHERE entity_type = ? AND entity_id = ?")
                    .bind(&entity_type)
                    .bind(&entity_id)
                    .execute(&mut *conn)
                    .await?;
            } else {
                sqlx::query(
                    "INSERT INTO remote_rating (entity_type, entity_id, rating, updated_at)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT(entity_type, entity_id) DO UPDATE
                        SET rating = excluded.rating, updated_at = excluded.updated_at",
                )
                .bind(&entity_type)
                .bind(&entity_id)
                .bind(clamp_rating(rating))
                .bind(change.changed_at)
                .execute(&mut *conn)
                .await?;
            }
            Ok(Outcome::Applied)
        }

        ("scrobble", _) => {
            let track_id = payload_str(&change.payload, "track_id")
                .unwrap_or(&change.entity_id)
                .to_string();
            let played_at = change
                .payload
                .get("played_at")
                .and_then(Value::as_i64)
                .unwrap_or(change.changed_at);
            let submission = change
                .payload
                .get("submission")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            sqlx::query(
                "INSERT OR REPLACE INTO remote_history (track_remote_id, played_at, submission)
                 VALUES (?, ?, ?)",
            )
            .bind(&track_id)
            .bind(played_at)
            .bind(i64::from(submission))
            .execute(&mut *conn)
            .await?;
            Ok(Outcome::Applied)
        }

        ("queue", "upsert") => {
            sqlx::query(
                "INSERT INTO remote_queue
                    (id, current_remote_id, position_ms, changed_by, updated_at)
                 VALUES (1, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                    current_remote_id = excluded.current_remote_id,
                    position_ms       = excluded.position_ms,
                    changed_by        = excluded.changed_by,
                    updated_at        = excluded.updated_at",
            )
            .bind(payload_str(&change.payload, "current"))
            .bind(
                change
                    .payload
                    .get("position_ms")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .max(0),
            )
            .bind(payload_str(&change.payload, "client"))
            .bind(change.changed_at)
            .execute(&mut *conn)
            .await?;

            if let Some(ids) = payload_ids(&change.payload, "track_ids") {
                sqlx::query("DELETE FROM remote_queue_track")
                    .execute(&mut *conn)
                    .await?;
                for (position, id) in ids.iter().enumerate() {
                    sqlx::query(
                        "INSERT INTO remote_queue_track (position, track_remote_id) VALUES (?, ?)",
                    )
                    .bind(position as i64)
                    .bind(id)
                    .execute(&mut *conn)
                    .await?;
                }
            }
            Ok(Outcome::Applied)
        }

        ("share", "upsert") => apply_share_upsert(conn, change).await,
        ("share", "delete") => {
            sqlx::query("DELETE FROM remote_share_track WHERE share_remote_id = ?")
                .bind(&change.entity_id)
                .execute(&mut *conn)
                .await?;
            sqlx::query("DELETE FROM remote_share WHERE remote_id = ?")
                .bind(&change.entity_id)
                .execute(&mut *conn)
                .await?;
            Ok(Outcome::Applied)
        }

        // A newer server talking to an older client. Skip and keep the
        // cursor moving — this is expected, not a fault.
        _ => Ok(Outcome::Ignored),
    }
}

async fn delete_favorite(
    conn: &mut SqliteConnection,
    entity_type: &str,
    entity_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM remote_favorite WHERE entity_type = ? AND entity_id = ?")
        .bind(entity_type)
        .bind(entity_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Playlist upsert, field by field.
///
/// The insert plants a row with defaults if none exists; each `UPDATE`
/// afterwards fires only for a key the payload actually carries. That is
/// what keeps `create_playlist` — which sends neither `comment` nor
/// `public` — from blanking values a later `update_playlist` had set.
async fn apply_playlist_upsert(
    conn: &mut SqliteConnection,
    change: &SyncChange,
) -> AppResult<Outcome> {
    let id = payload_str(&change.payload, "id")
        .unwrap_or(&change.entity_id)
        .to_string();

    sqlx::query(
        "INSERT INTO remote_playlist (remote_id, name, is_public, created_at, updated_at)
         VALUES (?, ?, 0, ?, ?)
         ON CONFLICT(remote_id) DO NOTHING",
    )
    .bind(&id)
    .bind(payload_str(&change.payload, "name").unwrap_or_default())
    .bind(change.changed_at)
    .bind(change.changed_at)
    .execute(&mut *conn)
    .await?;

    if let Some(name) = payload_str(&change.payload, "name") {
        sqlx::query("UPDATE remote_playlist SET name = ? WHERE remote_id = ?")
            .bind(name)
            .bind(&id)
            .execute(&mut *conn)
            .await?;
    }
    // Present-and-null is a real clear, so this branch keys on the key
    // existing, not on the value being a string.
    if change.payload.get("comment").is_some() {
        sqlx::query("UPDATE remote_playlist SET comment = ? WHERE remote_id = ?")
            .bind(payload_str(&change.payload, "comment"))
            .bind(&id)
            .execute(&mut *conn)
            .await?;
    }
    if let Some(public) = change.payload.get("public").and_then(Value::as_bool) {
        sqlx::query("UPDATE remote_playlist SET is_public = ? WHERE remote_id = ?")
            .bind(i64::from(public))
            .bind(&id)
            .execute(&mut *conn)
            .await?;
    }

    sqlx::query("UPDATE remote_playlist SET updated_at = ? WHERE remote_id = ?")
        .bind(change.changed_at)
        .bind(&id)
        .execute(&mut *conn)
        .await?;

    if let Some(ids) = payload_ids(&change.payload, "track_ids") {
        sqlx::query("DELETE FROM remote_playlist_track WHERE playlist_remote_id = ?")
            .bind(&id)
            .execute(&mut *conn)
            .await?;
        for (position, track_id) in ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO remote_playlist_track
                    (playlist_remote_id, position, track_remote_id)
                 VALUES (?, ?, ?)",
            )
            .bind(&id)
            .bind(position as i64)
            .bind(track_id)
            .execute(&mut *conn)
            .await?;
        }
    }

    Ok(Outcome::Applied)
}

/// Share upsert. Same discipline as the playlist, and it matters here
/// for one specific reason: updating a share's description emits a
/// payload with **no** `track_ids`, so writing them unconditionally
/// would empty the share.
async fn apply_share_upsert(conn: &mut SqliteConnection, change: &SyncChange) -> AppResult<Outcome> {
    let id = payload_str(&change.payload, "id")
        .unwrap_or(&change.entity_id)
        .to_string();

    sqlx::query(
        "INSERT INTO remote_share (remote_id, created_at, updated_at)
         VALUES (?, ?, ?)
         ON CONFLICT(remote_id) DO NOTHING",
    )
    .bind(&id)
    .bind(change.changed_at)
    .bind(change.changed_at)
    .execute(&mut *conn)
    .await?;

    if change.payload.get("description").is_some() {
        sqlx::query("UPDATE remote_share SET description = ? WHERE remote_id = ?")
            .bind(payload_str(&change.payload, "description"))
            .bind(&id)
            .execute(&mut *conn)
            .await?;
    }
    if change.payload.get("expires_at").is_some() {
        sqlx::query("UPDATE remote_share SET expires_at = ? WHERE remote_id = ?")
            .bind(change.payload.get("expires_at").and_then(Value::as_i64))
            .bind(&id)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query("UPDATE remote_share SET updated_at = ? WHERE remote_id = ?")
        .bind(change.changed_at)
        .bind(&id)
        .execute(&mut *conn)
        .await?;

    if let Some(ids) = payload_ids(&change.payload, "track_ids") {
        replace_share_tracks(conn, &id, &ids).await?;
    }

    Ok(Outcome::Applied)
}

async fn replace_share_tracks(
    conn: &mut SqliteConnection,
    share_id: &str,
    track_ids: &[String],
) -> AppResult<()> {
    sqlx::query("DELETE FROM remote_share_track WHERE share_remote_id = ?")
        .bind(share_id)
        .execute(&mut *conn)
        .await?;
    for (position, track_id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO remote_share_track (share_remote_id, position, track_remote_id)
             VALUES (?, ?, ?)",
        )
        .bind(share_id)
        .bind(position as i64)
        .bind(track_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Read a string field. `None` covers both "absent" and "null" — every
/// caller that needs to tell them apart checks `payload.get(key)` first.
fn payload_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

/// Read an ordered list of identifiers, or `None` when the key is
/// absent. An empty array is a real emptying and comes back as
/// `Some(vec![])`.
fn payload_ids(payload: &Value, key: &str) -> Option<Vec<String>> {
    let array = payload.get(key)?.as_array()?;
    Some(
        array
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::dto::SyncPage;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::{Row, SqlitePool};

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        for migration in [
            include_str!(
                "../../../../migrations/profile/20260810120000_remote_source_projection.sql"
            ),
            include_str!("../../../../migrations/profile/20260810140000_remote_track_cache.sql"),
            include_str!("../../../../migrations/profile/20260813090000_remote_track_full_hash.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&pool).await.unwrap();
        }
        pool
    }

    fn change(entity_type: &str, action: &str, entity_id: &str, payload: Value) -> SyncChange {
        SyncChange {
            cursor: 1,
            entity_type: entity_type.into(),
            entity_id: entity_id.into(),
            action: action.into(),
            payload,
            origin_device_id: None,
            changed_at: 1_000,
        }
    }

    async fn playlist_row(conn: &mut SqliteConnection, id: &str) -> (String, Option<String>, i64) {
        let row = sqlx::query("SELECT name, comment, is_public FROM remote_playlist WHERE remote_id = ?")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        (
            row.try_get("name").unwrap(),
            row.try_get("comment").unwrap(),
            row.try_get("is_public").unwrap(),
        )
    }

    #[tokio::test]
    async fn creating_a_playlist_after_an_update_does_not_blank_what_the_update_set() {
        // The bug this guards: `create_playlist` emits neither `comment`
        // nor `public`, so an apply that wrote every field would erase
        // both every time a playlist is re-created or replayed.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        apply_change(
            &mut conn,
            &change(
                "playlist",
                "upsert",
                "p1",
                serde_json::json!({
                    "id": "p1", "name": "Mix", "comment": "notes",
                    "public": true, "track_ids": ["t1"]
                }),
            ),
        )
        .await
        .unwrap();

        // Now the create-shaped payload: three keys only.
        apply_change(
            &mut conn,
            &change(
                "playlist",
                "upsert",
                "p1",
                serde_json::json!({"id": "p1", "name": "Mix renamed", "track_ids": ["t1", "t2"]}),
            ),
        )
        .await
        .unwrap();

        let (name, comment, public) = playlist_row(&mut conn, "p1").await;
        assert_eq!(name, "Mix renamed");
        assert_eq!(comment.as_deref(), Some("notes"), "comment was blanked");
        assert_eq!(public, 1, "public was reset");
    }

    #[tokio::test]
    async fn a_null_comment_is_a_real_clear() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        apply_change(
            &mut conn,
            &change(
                "playlist",
                "upsert",
                "p1",
                serde_json::json!({"id": "p1", "name": "Mix", "comment": "notes"}),
            ),
        )
        .await
        .unwrap();
        apply_change(
            &mut conn,
            &change(
                "playlist",
                "upsert",
                "p1",
                serde_json::json!({"id": "p1", "name": "Mix", "comment": null}),
            ),
        )
        .await
        .unwrap();

        let (_, comment, _) = playlist_row(&mut conn, "p1").await;
        assert_eq!(comment, None, "an explicit null must clear, not be ignored");
    }

    #[tokio::test]
    async fn a_playlist_keeps_a_track_that_appears_twice() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        apply_change(
            &mut conn,
            &change(
                "playlist",
                "upsert",
                "p1",
                serde_json::json!({"id": "p1", "name": "M", "track_ids": ["t1", "t1", "t2"]}),
            ),
        )
        .await
        .unwrap();

        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT track_remote_id FROM remote_playlist_track
              WHERE playlist_remote_id = 'p1' ORDER BY position",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(ids, vec!["t1", "t1", "t2"]);
    }

    #[tokio::test]
    async fn updating_a_share_description_does_not_empty_it() {
        // `update_share` emits `{id, description, expires_at}` and no
        // track list. Writing tracks unconditionally would drop them.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        apply_change(
            &mut conn,
            &change(
                "share",
                "upsert",
                "s1",
                serde_json::json!({"id": "s1", "track_ids": ["t1", "t2"], "description": "a"}),
            ),
        )
        .await
        .unwrap();
        apply_change(
            &mut conn,
            &change(
                "share",
                "upsert",
                "s1",
                serde_json::json!({"id": "s1", "description": "b", "expires_at": null}),
            ),
        )
        .await
        .unwrap();

        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM remote_share_track WHERE share_remote_id='s1'")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(count, 2, "the share lost its tracks on a description edit");
    }

    #[tokio::test]
    async fn a_zero_rating_clears_instead_of_storing_zero() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        apply_change(
            &mut conn,
            &change(
                "rating",
                "upsert",
                "t1",
                serde_json::json!({"entity_type": "track", "entity_id": "t1", "rating": 4}),
            ),
        )
        .await
        .unwrap();
        apply_change(
            &mut conn,
            &change(
                "rating",
                "delete",
                "t1",
                serde_json::json!({"entity_type": "track", "entity_id": "t1", "rating": 0}),
            ),
        )
        .await
        .unwrap();

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_rating")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn an_out_of_range_rating_is_clamped_rather_than_aborting() {
        // Aborting would force a fresh snapshot whose replay hits the
        // same row — an unbreakable loop over one bad field.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        apply_change(
            &mut conn,
            &change(
                "rating",
                "upsert",
                "t1",
                serde_json::json!({"entity_type": "track", "entity_id": "t1", "rating": 99}),
            ),
        )
        .await
        .unwrap();

        let rating: i64 = sqlx::query_scalar("SELECT rating FROM remote_rating")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(rating, 5);
    }

    #[tokio::test]
    async fn an_unstarred_favorite_upsert_removes_the_row() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        apply_change(
            &mut conn,
            &change(
                "favorite",
                "upsert",
                "t1",
                serde_json::json!({"entity_type": "track", "entity_id": "t1", "starred": true}),
            ),
        )
        .await
        .unwrap();
        apply_change(
            &mut conn,
            &change(
                "favorite",
                "upsert",
                "t1",
                serde_json::json!({"entity_type": "track", "entity_id": "t1", "starred": false}),
            ),
        )
        .await
        .unwrap();

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_favorite")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn two_plays_of_one_track_are_distinct_but_a_replay_is_not() {
        // There is no scrobble identifier in the protocol; the natural
        // key is the only thing that separates two plays.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        for played_at in [1_000, 2_000, 1_000] {
            apply_change(
                &mut conn,
                &change(
                    "scrobble",
                    "append",
                    "t1",
                    serde_json::json!({"track_id": "t1", "submission": true, "played_at": played_at}),
                ),
            )
            .await
            .unwrap();
        }

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_history")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn an_unknown_entity_is_skipped_not_failed() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let outcome = apply_change(
            &mut conn,
            &change("something_new", "upsert", "x", serde_json::json!({})),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Ignored);

        let outcome = apply_change(
            &mut conn,
            &change("playlist", "reorder_v2", "p1", serde_json::json!({})),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Ignored);
    }

    #[tokio::test]
    async fn a_snapshot_replaces_everything_and_caches_its_songs() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        // Something stale to be swept away.
        apply_change(
            &mut conn,
            &change(
                "playlist",
                "upsert",
                "old",
                serde_json::json!({"id": "old", "name": "Stale", "track_ids": ["z"]}),
            ),
        )
        .await
        .unwrap();

        let snapshot: SyncSnapshot = serde_json::from_value(serde_json::json!({
            "cursor": 12,
            "playlists": [{
                "id": "p1", "name": "Mix", "public": false, "created_at": 1, "updated_at": 2,
                "songs": [{"id": "t1", "title": "Song", "artist": "A", "duration_ms": 1000}]
            }],
            "favorites": [{"entity_type": "track", "entity_id": "t1", "starred_at": 5}],
            "ratings": [{"entity_type": "track", "entity_id": "t1", "rating": 3, "updated_at": 5}],
            "history": [{"track_id": "t1", "submission": true, "played_at": 9}],
            "queue": {"songs": [{"id": "t1", "title": "Song"}], "current": "t1",
                      "position_ms": 4200, "updated_at": 7},
            "shares": [{"id": "s1", "track_ids": ["t1"], "visit_count": 2}]
        }))
        .unwrap();

        apply_snapshot(&mut conn, &snapshot).await.unwrap();

        let stale: i64 =
            sqlx::query_scalar("SELECT count(*) FROM remote_playlist WHERE remote_id = 'old'")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(stale, 0, "the snapshot did not replace the old projection");

        let title: String =
            sqlx::query_scalar("SELECT title FROM remote_track WHERE remote_id = 't1'")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(title, "Song", "the snapshot's song metadata was not cached");

        // Nothing is missing: the snapshot carried every song it
        // referenced.
        assert!(missing_track_ids(&mut conn).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_snapshot_does_not_destroy_a_playlist_created_offline() {
        // The creation is still queued, so the server has never heard of
        // this playlist and its snapshot cannot be evidence that it was
        // deleted.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let local_id = crate::remote::mutation::new_placeholder();

        sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES (?, 'Offline mix')")
            .bind(&local_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO remote_playlist_track VALUES (?, 0, 't1')")
            .bind(&local_id)
            .execute(&mut *conn)
            .await
            .unwrap();

        let snapshot: SyncSnapshot = serde_json::from_str(r#"{"cursor": 9}"#).unwrap();
        apply_snapshot(&mut conn, &snapshot).await.unwrap();

        let survived: i64 =
            sqlx::query_scalar("SELECT count(*) FROM remote_playlist WHERE remote_id = ?")
                .bind(&local_id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(survived, 1, "an unsent offline playlist was destroyed");

        let tracks: i64 =
            sqlx::query_scalar("SELECT count(*) FROM remote_playlist_track WHERE playlist_remote_id = ?")
                .bind(&local_id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(tracks, 1, "its tracks went with it");
    }

    #[tokio::test]
    async fn identifiers_from_a_change_event_show_up_as_missing_metadata() {
        // A change carries bare identifiers, so this is the gap the
        // orchestrator has to close by fetching.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        apply_change(
            &mut conn,
            &change(
                "playlist",
                "upsert",
                "p1",
                serde_json::json!({"id": "p1", "name": "M", "track_ids": ["t9"]}),
            ),
        )
        .await
        .unwrap();

        assert_eq!(missing_track_ids(&mut conn).await.unwrap(), vec!["t9"]);
    }

    /// Captured verbatim from a running server (2026-08-10) seeded with
    /// a real six-track library, then driven through every entity the
    /// protocol carries: two favourites (a track and an album), two
    /// ratings, two scrobbles, a saved queue, a playlist created then
    /// updated, and a share.
    ///
    /// Bytes the server produced, not a shape we imagined — the only
    /// kind of fixture that can catch us having imagined the wrong one.
    const REAL_SNAPSHOT: &str = include_str!("testdata/snapshot.json");
    const REAL_CHANGES: &str = include_str!("testdata/changes.json");

    /// Everything the projection holds, in a form two feeds can be
    /// compared on.
    async fn projected_state(conn: &mut SqliteConnection) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        for (label, sql) in [
            (
                "playlist",
                "SELECT remote_id || '|' || name || '|' || COALESCE(comment, '~') || '|' || is_public
                   FROM remote_playlist ORDER BY remote_id",
            ),
            (
                "playlist_track",
                "SELECT playlist_remote_id || '|' || position || '|' || track_remote_id
                   FROM remote_playlist_track ORDER BY playlist_remote_id, position",
            ),
            (
                "favorite",
                "SELECT entity_type || '|' || entity_id FROM remote_favorite
                  ORDER BY entity_type, entity_id",
            ),
            (
                "rating",
                "SELECT entity_type || '|' || entity_id || '|' || rating FROM remote_rating
                  ORDER BY entity_type, entity_id",
            ),
            (
                "history",
                "SELECT track_remote_id || '|' || played_at || '|' || submission
                   FROM remote_history ORDER BY played_at",
            ),
            (
                "queue",
                "SELECT COALESCE(current_remote_id, '~') || '|' || position_ms FROM remote_queue",
            ),
            (
                "queue_track",
                "SELECT position || '|' || track_remote_id FROM remote_queue_track ORDER BY position",
            ),
            (
                "share",
                "SELECT remote_id || '|' || COALESCE(description, '~') FROM remote_share
                  ORDER BY remote_id",
            ),
            (
                "share_track",
                "SELECT share_remote_id || '|' || position || '|' || track_remote_id
                   FROM remote_share_track ORDER BY share_remote_id, position",
            ),
        ] {
            let values: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
                .fetch_all(&mut *conn)
                .await
                .unwrap();
            for value in values {
                rows.push((label.to_string(), value));
            }
        }
        rows
    }

    #[tokio::test]
    async fn the_servers_own_snapshot_applies_across_every_entity() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let snapshot: SyncSnapshot = serde_json::from_str(REAL_SNAPSHOT).unwrap();

        apply_snapshot(&mut conn, &snapshot).await.unwrap();

        let state = projected_state(&mut conn).await;
        let count = |label: &str| state.iter().filter(|(l, _)| l == label).count();
        assert_eq!(count("playlist"), 1);
        assert_eq!(count("playlist_track"), 3);
        assert_eq!(count("favorite"), 2, "a track and an album were starred");
        assert_eq!(count("rating"), 2);
        assert_eq!(count("history"), 2);
        assert_eq!(count("queue"), 1);
        assert_eq!(count("queue_track"), 3);
        assert_eq!(count("share"), 1);
        assert_eq!(count("share_track"), 2);

        // The snapshot carries whole song objects, so nothing is left
        // waiting for metadata.
        assert!(missing_track_ids(&mut conn).await.unwrap().is_empty());
        let cached: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_track")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert!(cached >= 3, "the snapshot's songs were not cached");
    }

    #[tokio::test]
    async fn replaying_the_servers_own_journal_reaches_the_snapshot_state() {
        // The two feeds must agree. If they do not, what the user sees
        // depends on whether they bootstrapped or caught up — which is
        // the kind of divergence nobody can debug from a screenshot.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        let page: SyncPage = serde_json::from_str(REAL_CHANGES).unwrap();
        assert_eq!(page.changes.len(), 10, "the capture lost events");
        for change in &page.changes {
            assert_eq!(
                apply_change(&mut conn, change).await.unwrap(),
                Outcome::Applied,
                "event {} ({}/{}) was not understood",
                change.cursor,
                change.entity_type,
                change.action
            );
        }
        let from_journal = projected_state(&mut conn).await;

        // Same database, replaced by the snapshot.
        let snapshot: SyncSnapshot = serde_json::from_str(REAL_SNAPSHOT).unwrap();
        apply_snapshot(&mut conn, &snapshot).await.unwrap();
        let from_snapshot = projected_state(&mut conn).await;

        assert_eq!(
            from_journal, from_snapshot,
            "walking the journal and taking a snapshot produced different state"
        );
    }

    #[tokio::test]
    async fn a_sparser_sighting_does_not_blank_richer_metadata() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let rich: SongItem = serde_json::from_value(serde_json::json!({
            "id": "t1", "title": "Song", "artist": "A", "album": "B", "duration_ms": 1234
        }))
        .unwrap();
        let sparse: SongItem =
            serde_json::from_value(serde_json::json!({"id": "t1", "title": "Song"})).unwrap();

        cache_song(&mut conn, &rich).await.unwrap();
        cache_song(&mut conn, &sparse).await.unwrap();

        let row = sqlx::query("SELECT artist, duration_ms FROM remote_track WHERE remote_id='t1'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(row.try_get::<Option<String>, _>("artist").unwrap().as_deref(), Some("A"));
        assert_eq!(row.try_get::<i64, _>("duration_ms").unwrap(), 1234);
    }
}
