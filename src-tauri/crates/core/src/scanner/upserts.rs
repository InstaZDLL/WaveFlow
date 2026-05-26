//! SQL upserts that the scanner uses to write tracks / artists / albums
//! / artwork / genres back into the per-profile database, plus the
//! post-scan `merge_implicit_compilations` pass.
//!
//! Every helper takes `&mut sqlx::SqliteConnection` so it can
//! participate in the open transaction `scan_folder_inner` runs across
//! a batch — never reach for the pool from inside these to avoid
//! breaking the single-writer discipline (see CLAUDE.md "Single writer
//! to SQLite").

use std::path::Path;

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::CoreResult;

use super::extract::{extract_artist_image, ExtractedCover};

/// Sentinel album-artist row used when an album is tagged as a
/// compilation but has no explicit Album Artist. Resolved to a real
/// `artist` row on first encounter via [`upsert_artist`], then reused.
pub const VARIOUS_ARTISTS_LABEL: &str = "Various Artists";

pub fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// Normalize a title/name for dedup purposes: lowercase, strip punctuation
/// and collapse whitespace. Good enough to match "The Beatles" / "THE  BEATLES"
/// or "the beatles!" onto a single canonical key without pulling in a proper
/// Unicode normalization library.
pub fn canonical_name(s: &str) -> String {
    s.trim()
        .chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split a raw artist string like `"Elior, DJ Garlik"` into individual
/// names. Conservative: only splits on `", "` and `"; "` so that artist
/// names containing `&`, `/`, or `feat.` (e.g. `"AC/DC"`, `"Simon &
/// Garfunkel"`) stay intact.
///
/// Returns the trimmed, non-empty names in the order they appeared —
/// the first entry is treated as the primary artist by the caller.
pub fn split_artist_name(raw: &str) -> Vec<String> {
    raw.split([',', ';'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Upsert an artwork row keyed on its content hash. Existing rows are
/// returned as-is; new rows are inserted with the caller-supplied source
/// label (`embedded`, `folder`, `deezer`, `user`...) so a future cleanup
/// job can distinguish scanner-extracted art from remote/manual files.
pub async fn upsert_artwork(
    conn: &mut sqlx::SqliteConnection,
    hash: &str,
    format: &str,
    source: &str,
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
            .bind(source)
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

    let result = sqlx::query("INSERT INTO artist (name, canonical_name) VALUES (?, ?)")
        .bind(name)
        .bind(&canon)
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
        "INSERT INTO album (title, canonical_title, artist_id, year, album_artist, is_compilation)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(title)
    .bind(&canon)
    .bind(artist_id)
    .bind(year)
    .bind(album_artist_display.as_deref())
    .bind(if is_compilation { 1_i64 } else { 0_i64 })
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

    let existing: Option<i64> =
        sqlx::query_scalar("SELECT id FROM genre WHERE canonical_name = ?")
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

/// Best-effort: link a freshly resolved local artist image to its `artist`
/// row when the row has no artwork yet. Idempotent — re-running with a
/// already-linked artist is a no-op (the `IS NULL` guard prevents
/// overwriting a manually uploaded picture).
pub async fn link_local_artist_image(
    conn: &mut sqlx::SqliteConnection,
    artist_id: i64,
    cover: &ExtractedCover,
) -> CoreResult<()> {
    let artwork_id = upsert_artwork(conn, &cover.hash, &cover.format, cover.source).await?;
    sqlx::query("UPDATE artist SET artwork_id = ? WHERE id = ? AND artwork_id IS NULL")
        .bind(artwork_id)
        .bind(artist_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Walk every artist name parsed from `raw`, pair it with its `artist.id`
/// from `artist_ids` (positionally aligned), and try to resolve a sidecar
/// artist image from `track_path`. Idempotent — artists that already have
/// an `artwork_id` are skipped by [`link_local_artist_image`].
///
/// Skips the "Various Artists" sentinel: a compilation folder never holds
/// a meaningful artist photo and we'd just pin a random album cover to it.
pub async fn maybe_link_artist_images(
    conn: &mut sqlx::SqliteConnection,
    artist_raw: Option<&str>,
    artist_ids: &[i64],
    track_path: &Path,
    artwork_dir: &Path,
) -> CoreResult<()> {
    let Some(raw) = artist_raw else {
        return Ok(());
    };
    let names = split_artist_name(raw);
    let va_canon = canonical_name(VARIOUS_ARTISTS_LABEL);
    for (name, id) in names.iter().zip(artist_ids.iter()) {
        let canon = canonical_name(name);
        if canon.is_empty() || canon == va_canon {
            continue;
        }
        // Cheap pre-check so we don't walk the FS when the artist already
        // has artwork (Deezer fetch, manual upload, or earlier scan).
        let has_artwork: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM artist WHERE id = ? AND artwork_id IS NOT NULL")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?;
        if has_artwork.is_some() {
            continue;
        }
        if let Some(cover) = extract_artist_image(track_path, &canon, artwork_dir) {
            link_local_artist_image(&mut *conn, *id, &cover).await?;
        }
    }
    Ok(())
}

/// Post-scan pass that promotes "tagless" same-title album rows into a
/// single Various-Artists compilation. Catches the common case where a
/// lofi / mood / cover-pack compilation (Soothing Breeze, Coffee Shop,
/// etc.) ships without `aART` and without `TCMP` so each track lands
/// in its own primary-artist-keyed album row.
///
/// Heuristic — conservative to avoid false positives on legit cases
/// like "two different artists who self-titled":
///   - same `canonical_title`
///   - every row has `album_artist IS NULL AND is_compilation = 0`
///     (the tag-driven path is the source of truth — never override)
///   - at least 3 distinct `artist_id`s (so a featuring on one track
///     of a normal album doesn't get promoted to a fake compilation)
///
/// On match: pick the lowest-id row as survivor, set
/// `(artist_id = VariousArtists, album_artist = "Various Artists",
/// is_compilation = 1)`, reparent every track of the sibling rows
/// onto the survivor, and delete the siblings. Their artwork rows
/// stay around (other albums may share them via hash dedup).
pub async fn merge_implicit_compilations(pool: &SqlitePool) -> CoreResult<()> {
    let groups: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT canonical_title
          FROM album
         WHERE album_artist IS NULL
           AND is_compilation = 0
           AND artist_id IS NOT NULL
         GROUP BY canonical_title
        HAVING COUNT(DISTINCT artist_id) >= 3
        "#,
    )
    .fetch_all(pool)
    .await?;

    if groups.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    let va_id = upsert_artist(&mut tx, VARIOUS_ARTISTS_LABEL).await?;
    let Some(va_id) = va_id else {
        // upsert_artist returned None — name canonicalised to empty,
        // shouldn't happen with "Various Artists" but guard defensively.
        tx.rollback().await?;
        return Ok(());
    };

    for (canonical_title,) in groups {
        let album_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM album
              WHERE canonical_title = ?
                AND album_artist IS NULL
                AND is_compilation = 0
              ORDER BY id ASC",
        )
        .bind(&canonical_title)
        .fetch_all(&mut *tx)
        .await?;

        let Some((&survivor, siblings)) = album_ids.split_first() else {
            continue;
        };
        if siblings.is_empty() {
            continue;
        }

        sqlx::query(
            "UPDATE album
                SET artist_id    = ?,
                    album_artist = ?,
                    is_compilation = 1
              WHERE id = ?",
        )
        .bind(va_id)
        .bind(VARIOUS_ARTISTS_LABEL)
        .bind(survivor)
        .execute(&mut *tx)
        .await?;

        for sid in siblings {
            sqlx::query("UPDATE track SET album_id = ? WHERE album_id = ?")
                .bind(survivor)
                .bind(sid)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM album WHERE id = ?")
                .bind(sid)
                .execute(&mut *tx)
                .await?;
        }

        tracing::info!(
            canonical_title = %canonical_title,
            survivor,
            merged = siblings.len(),
            "auto-merged implicit compilation"
        );
    }

    tx.commit().await?;
    Ok(())
}
