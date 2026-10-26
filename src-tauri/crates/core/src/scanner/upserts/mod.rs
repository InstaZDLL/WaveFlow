//! SQL upserts that the scanner uses to write tracks / artists / albums
//! / artwork / genres back into the per-profile database, plus the
//! post-scan `merge_implicit_compilations` pass.
//!
//! Every helper takes `&mut sqlx::SqliteConnection` so it can
//! participate in the open transaction `scan_folder_inner` runs across
//! a batch — never reach for the pool from inside these to avoid
//! breaking the single-writer discipline (see CLAUDE.md "Single writer
//! to SQLite").

use std::collections::HashMap;

use chrono::Utc;

use crate::error::CoreResult;

mod album_artist;
mod artist_images;
mod folder_covers;
mod play_events;

pub use album_artist::{merge_implicit_compilations, repoint_fallback_album_artist};
pub use artist_images::{
    link_local_artist_image, link_va_artist_image, maybe_link_artist_images, ArtistImageScanCache,
};
pub use folder_covers::refresh_folder_covers;
pub use play_events::reattach_orphaned_play_events;

/// Scan-scoped memo for the `artist` / `genre` lookups that otherwise
/// fire one `SELECT … WHERE canonical_name = ?` per track. A 900-track
/// library typically resolves to ~100 distinct artists and ~20 genres,
/// so without this the scanner's consumer loop pays thousands of
/// redundant single-writer round-trips.
///
/// Keyed on [`canonical_name`] exactly like [`upsert_artist`] /
/// [`upsert_genre`] so a cache hit returns the same id the SELECT
/// would. Ids stay valid across the scanner's periodic commits (the
/// rows they point at are committed, never rolled back — any error
/// aborts the whole scan and drops the cache with it). `album` is
/// deliberately NOT memoised: [`upsert_album`] carries sticky
/// compilation / album-artist backfill logic that must run per track.
#[derive(Default)]
pub struct UpsertCache {
    artists: HashMap<String, i64>,
    genres: HashMap<String, i64>,
}

impl UpsertCache {
    /// Cached [`upsert_artist`]. Mirrors its trim → `canonical_name` →
    /// empty-guard so the cache key matches the DB lookup key.
    pub async fn artist(
        &mut self,
        conn: &mut sqlx::SqliteConnection,
        raw_name: &str,
    ) -> CoreResult<Option<i64>> {
        let canon = canonical_name(raw_name.trim());
        if canon.is_empty() {
            return Ok(None);
        }
        if let Some(&id) = self.artists.get(&canon) {
            return Ok(Some(id));
        }
        let id = upsert_artist(conn, raw_name).await?;
        if let Some(id) = id {
            self.artists.insert(canon, id);
        }
        Ok(id)
    }

    /// Cached equivalent of [`upsert_artist_list`] — splits the raw
    /// multi-artist string the same way, then resolves each through
    /// [`Self::artist`].
    pub async fn artist_list(
        &mut self,
        conn: &mut sqlx::SqliteConnection,
        raw: &Option<String>,
    ) -> CoreResult<Vec<i64>> {
        let Some(raw) = raw else {
            return Ok(Vec::new());
        };
        let mut ids = Vec::new();
        for name in split_artist_name(raw) {
            if let Some(id) = self.artist(&mut *conn, &name).await? {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// Cached [`upsert_genre`] for each of `names`: the ids of the genres
    /// a track is tagged with, deduplicated (two spellings the split kept
    /// apart can still share a canonical row).
    pub async fn genre_list(
        &mut self,
        conn: &mut sqlx::SqliteConnection,
        names: &[String],
    ) -> CoreResult<Vec<i64>> {
        let mut ids = Vec::new();
        for name in names {
            if let Some(id) = self.genre(&mut *conn, name).await? {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    /// Cached [`upsert_genre`].
    pub async fn genre(
        &mut self,
        conn: &mut sqlx::SqliteConnection,
        raw_name: &str,
    ) -> CoreResult<Option<i64>> {
        let canon = canonical_name(raw_name.trim());
        if canon.is_empty() {
            return Ok(None);
        }
        if let Some(&id) = self.genres.get(&canon) {
            return Ok(Some(id));
        }
        let id = upsert_genre(conn, raw_name).await?;
        if let Some(id) = id {
            self.genres.insert(canon, id);
        }
        Ok(id)
    }
}

/// Sentinel album-artist row used when an album is tagged as a
/// compilation but has no explicit Album Artist. Resolved to a real
/// `artist` row on first encounter via [`upsert_artist`], then reused.
pub const VARIOUS_ARTISTS_LABEL: &str = "Various Artists";

pub fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

// `canonical_name` moved to `super::canonical` so the postgres-only
// build (which skips this whole `upserts` module) can still consume it
// from the always-compiled `extract` module. Re-exported here for
// backwards source compatibility with existing imports.
pub use super::canonical::canonical_name;
use super::extract::ArtworkSource;

/// Split a raw artist string into individual names. Only `"; "` is
/// honoured as a separator — the convention used by MusicBrainz Picard,
/// foobar2000, Beets and Mp3Tag for multi-value artist fields. We
/// deliberately do **not** split on `", "` even though plenty of
/// ad-hoc taggers use it, because a comma can be part of the name
/// itself (`"Tyler, The Creator"`, `"Earth, Wind & Fire"`,
/// `"Crosby, Stills, Nash & Young"`); the earlier comma split
/// silently fragmented those into multiple artists.
///
/// Libraries that stored multi-artist values comma-joined will see
/// every track listed under the combined-name artist; the user can
/// re-tag with `; ` (the round-trip is documented in CLAUDE.md and
/// `docs/features/library.md`) to opt back in to per-artist linking.
///
/// Returns the trimmed, non-empty names in the order they appeared —
/// the first entry is treated as the primary artist by the caller.
pub fn split_artist_name(raw: &str) -> Vec<String> {
    raw.split("; ")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Upsert an artwork row keyed on its content hash. Existing rows are
/// returned as-is; new rows are inserted with the caller's
/// [`ArtworkSource`] so a future cleanup job can tell scanner-extracted
/// art from remote/manual files.
pub async fn upsert_artwork(
    conn: &mut sqlx::SqliteConnection,
    hash: &str,
    format: &str,
    source: ArtworkSource,
) -> CoreResult<i64> {
    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM artwork WHERE hash = ?")
        .bind(hash)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(id) = existing {
        return Ok(id);
    }

    let now = now_millis();
    let result =
        sqlx::query("INSERT INTO artwork (hash, format, source, created_at) VALUES (?, ?, ?, ?)")
            .bind(hash)
            .bind(format)
            .bind(source.as_str())
            .bind(now)
            .execute(&mut *conn)
            .await?;
    Ok(result.last_insert_rowid())
}

pub async fn upsert_artist(
    conn: &mut sqlx::SqliteConnection,
    raw_name: &str,
) -> CoreResult<Option<i64>> {
    let name = raw_name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let canon = canonical_name(name);
    if canon.is_empty() {
        return Ok(None);
    }

    let existing: Option<i64> =
        sqlx::query_scalar("SELECT id FROM artist WHERE canonical_name = ?")
            .bind(&canon)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(id) = existing {
        return Ok(Some(id));
    }

    // `pinyin` travels with `canonical_name`, always: the rule is that
    // wherever one is written the other is too, which is why they sit in
    // the same module (#579).
    let result = sqlx::query("INSERT INTO artist (name, canonical_name, pinyin) VALUES (?, ?, ?)")
        .bind(name)
        .bind(&canon)
        .bind(super::canonical::pinyin_blob(name).unwrap_or_default())
        .execute(&mut *conn)
        .await?;
    Ok(Some(result.last_insert_rowid()))
}

/// Resolve the album-artist text to an `artist` row id, applying the
/// scanner's grouping policy:
///
/// 1. Explicit Album Artist tag → upsert that name verbatim.
/// 2. No tag but `is_compilation == true` → upsert the
///    `"Various Artists"` sentinel so a TCMP-flagged record stays
///    glued together regardless of per-track Artist diversity.
/// 3. No tag and not a compilation → fall back to the first artist of
///    the track (`track_primary_artist_id`), preserving the v1.0
///    behaviour for files the user hasn't re-tagged yet.
///
/// Returns the chosen `artist.id` plus the display text we want to
/// persist on `album.album_artist` (preserves the source casing).
pub async fn resolve_album_artist(
    conn: &mut sqlx::SqliteConnection,
    album_artist: Option<&str>,
    is_compilation: bool,
    track_primary_artist_id: Option<i64>,
) -> CoreResult<(Option<i64>, Option<String>)> {
    if let Some(name) = album_artist {
        let name = name.trim();
        if !name.is_empty() {
            let id = upsert_artist(conn, name).await?;
            return Ok((id, Some(name.to_string())));
        }
    }
    if is_compilation {
        let id = upsert_artist(conn, VARIOUS_ARTISTS_LABEL).await?;
        return Ok((id, Some(VARIOUS_ARTISTS_LABEL.to_string())));
    }
    Ok((track_primary_artist_id, None))
}

pub async fn upsert_album(
    conn: &mut sqlx::SqliteConnection,
    title: &str,
    album_artist_text: Option<&str>,
    is_compilation: bool,
    track_primary_artist_id: Option<i64>,
    year: Option<i64>,
) -> CoreResult<Option<i64>> {
    let title = title.trim();
    if title.is_empty() {
        return Ok(None);
    }
    let canon = canonical_name(title);
    if canon.is_empty() {
        return Ok(None);
    }

    let (artist_id, album_artist_display) = resolve_album_artist(
        conn,
        album_artist_text,
        is_compilation,
        track_primary_artist_id,
    )
    .await?;

    // The `UNIQUE (canonical_title, artist_id)` constraint treats NULL as
    // distinct in SQLite, so we dedup manually for the NULL-artist case.
    let mut existing: Option<i64> = if let Some(aid) = artist_id {
        sqlx::query_scalar("SELECT id FROM album WHERE canonical_title = ? AND artist_id = ?")
            .bind(&canon)
            .bind(aid)
            .fetch_optional(&mut *conn)
            .await?
    } else {
        sqlx::query_scalar("SELECT id FROM album WHERE canonical_title = ? AND artist_id IS NULL")
            .bind(&canon)
            .fetch_optional(&mut *conn)
            .await?
    };

    // Re-use an existing compilation row for this title even when the
    // incoming track has no Album Artist tag — without this, every
    // rescan of a previously auto-merged compilation would re-fragment
    // (the artist-specific SELECT above misses because the merged row
    // has artist_id = "Various Artists"). Only applies when the
    // incoming track has no explicit album_artist tag and isn't itself
    // flagged as compilation; otherwise the explicit fields take
    // precedence and the regular upsert path runs.
    if existing.is_none() && album_artist_text.is_none() && !is_compilation {
        existing = sqlx::query_scalar(
            "SELECT id FROM album
              WHERE canonical_title = ? AND is_compilation = 1
              LIMIT 1",
        )
        .bind(&canon)
        .fetch_optional(&mut *conn)
        .await?;
    }
    if let Some(id) = existing {
        // Backfill album_artist / is_compilation on the existing row
        // ONLY when this scan brings new information. Re-scans of files
        // without an Album Artist tag and without the compilation flag
        // skip the UPDATE entirely — preserves the v1.0 perf profile
        // for libraries the user hasn't re-tagged. The COALESCE / OR
        // keeps the values "sticky": once a track declares an album
        // artist or compilation, the row keeps it even if siblings
        // drop the tags.
        if album_artist_display.is_some() || is_compilation {
            sqlx::query(
                "UPDATE album
                    SET album_artist   = COALESCE(album_artist, ?),
                        is_compilation = CASE WHEN ? = 1 OR is_compilation = 1 THEN 1 ELSE 0 END
                  WHERE id = ?",
            )
            .bind(album_artist_display.as_deref())
            .bind(if is_compilation { 1_i64 } else { 0_i64 })
            .bind(id)
            .execute(&mut *conn)
            .await?;
        }
        return Ok(Some(id));
    }

    let result = sqlx::query(
        "INSERT INTO album (title, canonical_title, artist_id, year, album_artist,
                            is_compilation, pinyin)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(title)
    .bind(&canon)
    .bind(artist_id)
    .bind(year)
    .bind(album_artist_display.as_deref())
    .bind(if is_compilation { 1_i64 } else { 0_i64 })
    // Beside `canonical_title`, for the same reason (#579).
    .bind(super::canonical::pinyin_blob(title).unwrap_or_default())
    .execute(&mut *conn)
    .await?;
    Ok(Some(result.last_insert_rowid()))
}

/// Resolve a raw multi-artist string (e.g. `"A, B; C"`) to a vector of
/// artist row IDs. The first entry becomes the track's primary artist.
/// Empty / whitespace-only inputs yield an empty vector.
pub async fn upsert_artist_list(
    conn: &mut sqlx::SqliteConnection,
    raw: &Option<String>,
) -> CoreResult<Vec<i64>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    for name in split_artist_name(raw) {
        if let Some(id) = upsert_artist(&mut *conn, &name).await? {
            ids.push(id);
        }
    }
    Ok(ids)
}

pub async fn upsert_genre(
    conn: &mut sqlx::SqliteConnection,
    raw_name: &str,
) -> CoreResult<Option<i64>> {
    let name = raw_name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let canon = canonical_name(name);
    if canon.is_empty() {
        return Ok(None);
    }

    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM genre WHERE canonical_name = ?")
        .bind(&canon)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(id) = existing {
        return Ok(Some(id));
    }

    let result = sqlx::query("INSERT INTO genre (name, canonical_name) VALUES (?, ?)")
        .bind(name)
        .bind(&canon)
        .execute(&mut *conn)
        .await?;
    Ok(Some(result.last_insert_rowid()))
}
