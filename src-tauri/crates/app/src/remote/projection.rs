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
use waveflow_core::metadata::name_match::normalize_name;

use crate::{
    error::AppResult,
    remote::{
        dto::{SongItem, SyncChange, SyncSnapshot},
        mutation::{is_reserved_for_placeholders, PLACEHOLDER_GLOB},
    },
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
    for statement in [
        "DELETE FROM remote_playlist_track WHERE playlist_remote_id NOT GLOB ?",
        "DELETE FROM remote_playlist WHERE remote_id NOT GLOB ?",
        "DELETE FROM remote_share_track WHERE share_remote_id NOT GLOB ?",
        "DELETE FROM remote_share WHERE remote_id NOT GLOB ?",
    ] {
        sqlx::query(statement)
            .bind(PLACEHOLDER_GLOB)
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
        if is_reserved_for_placeholders(&playlist.id) {
            tracing::warn!("snapshot playlist skipped: its identifier is in the local namespace");
            continue;
        }
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
        if is_reserved_for_placeholders(&share.id) {
            tracing::warn!("snapshot share skipped: its identifier is in the local namespace");
            continue;
        }
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
            (remote_id, title, artist, artist_id, album, album_id, duration_ms, track_no, disc_no,
             year, genre, suffix, bitrate, size, artwork_hash, library_id, full_hash, cached_at,
             sort_artist, sort_album)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(remote_id) DO UPDATE SET
            -- A later cache pass for a bare id binds an empty title; keep the
            -- existing one rather than blanking a row we already labelled.
            title        = COALESCE(NULLIF(excluded.title, ''), title),
            artist       = COALESCE(excluded.artist, artist),
            artist_id    = COALESCE(excluded.artist_id, artist_id),
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
            cached_at    = excluded.cached_at,
            -- Keyed off the same COALESCE as the strings they are derived
            -- from: a sparser later sighting must not blank a key any more
            -- than it blanks the title it came from.
            sort_artist  = COALESCE(excluded.sort_artist, sort_artist),
            sort_album   = COALESCE(excluded.sort_album, sort_album)",
    )
    .bind(&song.id)
    // An untitled row is still better than no row: the playlist keeps
    // its ordering and the identifier is there to re-fetch with.
    .bind(song.title.as_deref().unwrap_or_default())
    .bind(song.artist.as_deref())
    .bind(song.artist_id.as_deref())
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
    // The comparison keys the unified track listing sorts artist and album
    // on, through the very normaliser the local half's `canonical_*` columns
    // went through. SQLite cannot fold a diacritic, so without these the two
    // halves interleave wrongly. The title is deliberately absent: it has no
    // canonical form locally either, so both halves sort it on their display
    // string and stay consistent that way.
    .bind(song.artist.as_deref().map(normalize_name))
    .bind(song.album.as_deref().map(normalize_name))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Write a track's corrected metadata over what the mirror holds.
///
/// ## Why this cannot be [`cache_song`]
///
/// That one coalesces every optional column, so a sparse later sighting
/// can never blank a field a richer earlier one filled in. The rule is
/// right for a mirror pass and **wrong here**: a patch that removes a
/// correction legitimately hands back a *smaller* value — a cleared
/// year comes back null, and coalescing would keep showing the year the
/// user just deleted, with no later pass able to correct it.
///
/// So these columns are written flat. They are exactly the fields
/// [`UpdateTrackMetadata`] can change; the rest of the row (size, hash,
/// artwork, library) is left alone, because correcting metadata moves
/// none of it.
///
/// A row that is absent is not created. The only caller patches a track
/// the mirror already holds, and minting a row from a reply would enter
/// a track into the catalogue by a path that never established it
/// belongs to a mirrored library.
///
/// [`UpdateTrackMetadata`]: super::mutation::Mutation::UpdateTrackMetadata
pub async fn apply_track_metadata(conn: &mut SqliteConnection, song: &SongItem) -> AppResult<()> {
    sqlx::query(
        "UPDATE remote_track
            SET title       = ?,
                artist      = ?,
                genre       = ?,
                year        = ?,
                track_no    = ?,
                disc_no     = ?,
                sort_artist = ?,
                cached_at   = ?
          WHERE remote_id = ?",
    )
    // Same fallback as the upsert: an untitled row still beats losing
    // the row's label, and the column is NOT NULL.
    .bind(song.title.as_deref().unwrap_or_default())
    .bind(song.artist.as_deref())
    .bind(song.genre.as_deref())
    .bind(song.year)
    .bind(song.track)
    .bind(song.disc)
    // Kept in step with the string it derives from. Leaving it stale
    // would sort the row under its old artist — exactly the
    // half-updated state the unified listing's sort keys exist to
    // prevent.
    .bind(song.artist.as_deref().map(normalize_name))
    .bind(chrono::Utc::now().timestamp_millis())
    .bind(&song.id)
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

/// Cached tracks that carry an artist name but no `artist_id` — the
/// signature of a row cached before the server exposed `artist_id` (the
/// column was added by a later migration and defaults NULL). Re-fetching
/// their metadata backfills the id so "About the artist" and the clickable
/// artist columns light up without a full re-snapshot.
///
/// Gated on `artist IS NOT NULL` so it converges: a track with a real
/// artist name resolves to an id on the next fetch and drops out. A track
/// with no artist at all is left alone — it has nothing to link to anyway.
/// Kept separate from [`missing_track_ids`] so the overview's
/// "awaiting metadata" count keeps meaning "no metadata at all".
pub async fn tracks_missing_artist_id(conn: &mut SqliteConnection) -> AppResult<Vec<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT remote_id FROM remote_track
          WHERE artist_id IS NULL AND artist IS NOT NULL",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows)
}

/// Apply one journal event.
pub async fn apply_change(conn: &mut SqliteConnection, change: &SyncChange) -> AppResult<Outcome> {
    // A playlist or share the server names inside the placeholder
    // namespace would be mistaken for — or would overwrite, or delete —
    // one created offline here. See `is_reserved_for_placeholders`.
    if matches!(change.entity_type.as_str(), "playlist" | "share") {
        let id = payload_str(&change.payload, "id").unwrap_or(&change.entity_id);
        if is_reserved_for_placeholders(id) || is_reserved_for_placeholders(&change.entity_id) {
            tracing::warn!(
                entity_type = %change.entity_type,
                "change ignored: its identifier is in the local namespace"
            );
            return Ok(Outcome::Ignored);
        }
    }
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
            // Same partial-update rule as playlist / share: an absent
            // "current" key means "unchanged", not "clear". Only overwrite
            // current_remote_id when the key is present (a present null still
            // clears it).
            let has_current = change.payload.get("current").is_some();
            sqlx::query(
                "INSERT INTO remote_queue
                    (id, current_remote_id, position_ms, changed_by, updated_at)
                 VALUES (1, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                    current_remote_id = CASE WHEN ?
                        THEN excluded.current_remote_id
                        ELSE current_remote_id END,
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
            .bind(has_current)
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
async fn apply_share_upsert(
    conn: &mut SqliteConnection,
    change: &SyncChange,
) -> AppResult<Outcome> {
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
mod tests;
