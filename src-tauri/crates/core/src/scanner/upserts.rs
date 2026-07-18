//! SQL upserts that the scanner uses to write tracks / artists / albums
//! / artwork / genres back into the per-profile database, plus the
//! post-scan `merge_implicit_compilations` pass.
//!
//! Every helper takes `&mut sqlx::SqliteConnection` so it can
//! participate in the open transaction `scan_folder_inner` runs across
//! a batch — never reach for the pool from inside these to avoid
//! breaking the single-writer discipline (see CLAUDE.md "Single writer
//! to SQLite").

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::{CoreError, CoreResult};

use super::extract::{
    extract_artist_image, extract_artist_image_cached, extract_folder_cover, ArtistImageDirCache,
    ExtractedCover,
};

/// Per-scan cache threaded through [`maybe_link_artist_images`]. Bundles
/// the two independent memos that make the sidecar-artist-image walk —
/// otherwise ~98 % of a first scan's DB time — cheap:
///
/// - `seen`: `(artist_id, parent dir)` pairs already attempted, so a
///   repeat artist in the same folder skips the match + has-artwork
///   probe entirely.
/// - `dirs`: each directory's image-candidate list, read once via
///   `read_dir` and reused across every artist that walks through it —
///   the lever for the common "shared folder, many distinct per-track
///   artists" layout (OST / compilation rips) where `seen` can't help
///   because every `(artist, folder)` pair is unique.
#[derive(Default)]
pub struct ArtistImageScanCache {
    seen: HashSet<(i64, PathBuf)>,
    dirs: ArtistImageDirCache,
}

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
/// Skips the "Various Artists" sentinel here because VA is an *album*
/// artist (it never appears in `track_artist`); its sidecar image is
/// resolved separately via the album relationship in [`link_va_artist_image`].
/// `cache` memoises this scan's sidecar-artist-image work — see
/// [`ArtistImageScanCache`]. The walk is by far the hottest part of a
/// first scan (`fs::read_dir` of up to 3 ancestor dirs + a
/// `canonical_name` per image entry, per artist, per track), nearly all
/// of it wasted on libraries with no local artist images. The cache
/// makes both axes cheap: a repeat `(artist, folder)` skips entirely,
/// and a never-seen `(artist, folder)` still reuses the cached
/// `read_dir` for any ancestor a sibling track already visited.
///
/// The `seen` skip is **per artist**, not per track, and keyed on the
/// parent dir, so a different folder of the same artist still resolves —
/// no per-album sidecar is missed. Callers thread one cache across the
/// whole scan; a one-off lookup can pass `&mut Default::default()`.
pub async fn maybe_link_artist_images(
    conn: &mut sqlx::SqliteConnection,
    artist_raw: Option<&str>,
    artist_ids: &[i64],
    track_path: &Path,
    artwork_dir: &Path,
    cache: &mut ArtistImageScanCache,
) -> CoreResult<()> {
    let Some(raw) = artist_raw else {
        return Ok(());
    };
    let names = split_artist_name(raw);
    let va_canon = canonical_name(VARIOUS_ARTISTS_LABEL);
    let parent = track_path.parent();
    for (name, id) in names.iter().zip(artist_ids.iter()) {
        // Per-(artist, folder) skip. `insert` returns false when the
        // pair was already attempted this scan — the match + the
        // has-artwork probe below are deterministic for it, so there's
        // nothing new to find.
        if let Some(p) = parent {
            if !cache.seen.insert((*id, p.to_path_buf())) {
                continue;
            }
        }
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
        if let Some(cover) =
            extract_artist_image_cached(track_path, &canon, artwork_dir, &mut cache.dirs)
        {
            link_local_artist_image(&mut *conn, *id, &cover).await?;
        }
    }
    Ok(())
}

/// Resolve a sidecar artist image for the "Various Artists" sentinel.
///
/// VA is an *album* artist — it's written to `album.artist_id` by
/// [`upsert_album`] / [`merge_implicit_compilations`] and never appears in
/// `track_artist` — so the per-track [`maybe_link_artist_images`] pass can't
/// reach it. A user who curates a `Various Artists/` folder and drops an
/// `artist.jpg` (or `Various Artists.jpg`) at its root legitimately wants
/// that photo on the VA page (issue #292); we resolve it here via the album
/// relationship instead.
///
/// Safe to run unconditionally: [`extract_artist_image`] only matches an
/// explicit artist-named sidecar (an `artist` / `performer` / `band` stem,
/// or a stem whose canonical form equals the artist name) — never a generic
/// `cover` / `folder` / `front` album cover — so this can't accidentally pin
/// a random album cover to VA. Idempotent: skips when no VA row exists and
/// (via the `artwork_id IS NULL` filter + [`link_local_artist_image`]'s own
/// guard) when VA already has artwork from a manual upload or earlier scan.
/// Returns `None` when there's no eligible VA candidate (no VA row, or VA
/// already has artwork), so callers don't count it as "considered". Returns
/// `Some(true)` when an eligible VA was linked to a sidecar, `Some(false)`
/// when an eligible VA had no sidecar to link.
pub async fn link_va_artist_image(
    conn: &mut sqlx::SqliteConnection,
    artwork_dir: &Path,
) -> CoreResult<Option<bool>> {
    let va_canon = canonical_name(VARIOUS_ARTISTS_LABEL);
    let va_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM artist WHERE canonical_name = ? AND artwork_id IS NULL")
            .bind(&va_canon)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(va_id) = va_id else {
        return Ok(None);
    };

    // VA tracks are linked through their album, not `track_artist`.
    let paths: Vec<(String,)> = sqlx::query_as(
        "SELECT t.file_path FROM track t
           JOIN album a ON a.id = t.album_id
          WHERE a.artist_id = ? AND t.is_available = 1
          LIMIT 16",
    )
    .bind(va_id)
    .fetch_all(&mut *conn)
    .await?;

    for (path,) in paths {
        if let Some(cover) = extract_artist_image(Path::new(&path), &va_canon, artwork_dir) {
            link_local_artist_image(&mut *conn, va_id, &cover).await?;
            return Ok(Some(true));
        }
    }
    Ok(Some(false))
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

/// Point an album at `artwork_id`, but only while its current artwork is
/// still sidecar-sourced (or absent). Returns whether the row changed.
///
/// [`refresh_folder_covers`] already filters on the same rule when it
/// builds its candidate list, but that read happens *before* the blocking
/// directory walk, which can run for seconds on a large library — long
/// enough for the user to upload a cover for this very album in the
/// meantime. Re-checking as part of the write closes that window: the
/// condition and the update are one statement, so nothing can slip
/// between them.
///
/// The boolean comes from `rows_affected`, i.e. what the database
/// actually changed rather than what the caller intended, so an album
/// that slipped out of the allowlist cannot inflate the scan summary.
async fn link_folder_cover_if_eligible(
    conn: &mut sqlx::SqliteConnection,
    album_id: i64,
    artwork_id: i64,
) -> CoreResult<bool> {
    let result = sqlx::query(
        "UPDATE album
            SET artwork_id = ?
          WHERE id = ?
            AND (
                artwork_id IS NULL
                OR EXISTS (
                    SELECT 1 FROM artwork aw
                     WHERE aw.id = album.artwork_id
                       AND aw.source = 'folder'
                )
            )",
    )
    .bind(artwork_id)
    .bind(album_id)
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Reconcile sidecar cover art against the albums that live next to it.
///
/// The scanner's fast path keys on the **audio** files' `(mtime, size)`,
/// so replacing `cover.jpg` in a folder changes nothing it looks at: the
/// tracks are skipped, `extract_folder_cover` never runs, and the album
/// keeps its old picture forever (issue #366, symptom A). The file that
/// changed simply isn't one the scanner watches.
///
/// This pass closes that gap by working per **directory** rather than
/// per file — one `read_dir` + one hash for a whole album, instead of
/// one per track — and updating any album whose sidecar no longer
/// matches what's stored. No extra bookkeeping is needed: `artwork.hash`
/// is already the blake3 of the picture bytes, so the stored row is its
/// own baseline for comparison.
///
/// Three deliberate restrictions:
///
/// - **Only sidecar-sourced artwork is replaced.** A row whose `source`
///   is anything other than `folder` was put there by something that
///   outranks a sidecar: `embedded` (extraction treats the sidecar as a
///   *fallback* for tracks whose tag carries no picture), `user` (a
///   manual upload), `deezer` (an enrichment fetch). The guard is an
///   allowlist — `folder` or no artwork at all — so a source added later
///   is preserved by default rather than silently clobbered. Caveat:
///   `artwork` rows are deduped on hash alone, so `source` records who
///   inserted the bytes first, not where this album got them. An image
///   that is embedded in one album and a sidecar next to another is
///   labelled `embedded` for both, and the sidecar one then stops being
///   refreshable here. See issue #401.
/// - **A deleted sidecar does not blank the album.** A vanished cover is
///   far more likely to be a transient state (files being reorganised)
///   than a request for a blank album.
/// - **A multi-directory album resolves against its first directory**
///   in sorted order, evaluated over *all* of the album's tracks rather
///   than only those under the scanned folder. Restricting it to the
///   scanned folder would make the winning directory depend on which
///   folder was scanned, i.e. flip the picture back and forth — exactly
///   the non-determinism this rule exists to prevent.
pub async fn refresh_folder_covers(
    pool: &SqlitePool,
    artwork_dir: &Path,
    folder_id: i64,
) -> CoreResult<u32> {
    // The subquery scopes the work to albums this scan could have
    // touched; the outer query then takes *every* available track of
    // those albums, so directory selection sees the album's full extent
    // no matter which folder triggered the scan.
    let rows: Vec<(i64, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT t.album_id, t.file_path, aw.hash, aw.source
           FROM track t
           JOIN album al ON al.id = t.album_id
           LEFT JOIN artwork aw ON aw.id = al.artwork_id
          WHERE t.is_available = 1
            AND t.album_id IN (
                SELECT DISTINCT album_id
                  FROM track
                 WHERE folder_id = ?
                   AND album_id IS NOT NULL
                   AND is_available = 1
            )",
    )
    .bind(folder_id)
    .fetch_all(pool)
    .await?;

    // album_id → (winning directory, current artwork hash, current source).
    // The directory is kept as a running minimum instead of collecting
    // every path and sorting at the end — same answer, no allocation
    // proportional to track count.
    let mut albums: HashMap<i64, (PathBuf, Option<String>, Option<String>)> = HashMap::new();
    for (album_id, file_path, hash, source) in rows {
        let Some(dir) = Path::new(&file_path).parent().map(Path::to_path_buf) else {
            continue;
        };
        albums
            .entry(album_id)
            .and_modify(|entry| {
                if dir < entry.0 {
                    entry.0 = dir.clone();
                }
            })
            .or_insert((dir, hash, source));
    }

    // Drop the albums this pass must not touch before doing any
    // filesystem work, so an untouchable album never costs a read.
    let candidates: Vec<(i64, PathBuf, Option<String>)> = albums
        .into_iter()
        .filter(|(_, (_, _, source))| {
            matches!(source.as_deref(), None | Some("folder"))
        })
        .map(|(album_id, (dir, hash, _))| (album_id, dir, hash))
        .collect();
    if candidates.is_empty() {
        return Ok(0);
    }

    // Resolve every distinct directory in one blocking batch. This is
    // `read_dir` + a full read + a blake3 per directory — hundreds of
    // megabytes on a large library — and must not run on the async
    // runtime, which would stall unrelated tasks (IPC replies, the
    // progress emitter) for seconds at the tail of every scan.
    //
    // Several albums commonly share one directory (a compilation split
    // across album rows, or singles dumped together), hence the dedup:
    // that's the read + hash we're trying not to repeat.
    let mut wanted: Vec<PathBuf> = candidates.iter().map(|(_, dir, _)| dir.clone()).collect();
    wanted.sort();
    wanted.dedup();

    let artwork_dir = artwork_dir.to_path_buf();
    let covers: HashMap<PathBuf, Option<(String, String)>> =
        tokio::task::spawn_blocking(move || {
            wanted
                .into_iter()
                .map(|dir| {
                    // `extract_folder_cover` takes a file path and looks
                    // at its parent, so hand it a synthetic child.
                    let found = extract_folder_cover(&dir.join("_"), &artwork_dir)
                        .map(|cover| (cover.hash, cover.format));
                    (dir, found)
                })
                .collect()
        })
        .await
        .map_err(|e| CoreError::Other(format!("folder cover resolution join: {e}")))?;

    // One transaction for the whole batch — the scanner is the single
    // writer and a per-album autocommit would serialise N round-trips
    // through WAL for no benefit. Also makes each artwork insert and the
    // album row that points at it land together.
    let mut tx = pool.begin().await?;
    let mut updated: u32 = 0;

    for (album_id, dir, current_hash) in candidates {
        let Some((hash, format)) = covers.get(&dir).cloned().flatten() else {
            continue;
        };
        if current_hash.as_deref() == Some(hash.as_str()) {
            continue;
        }

        let artwork_id = upsert_artwork(&mut tx, &hash, &format, "folder").await?;
        if !link_folder_cover_if_eligible(&mut tx, album_id, artwork_id).await? {
            tracing::debug!(album_id, "album cover changed under us; leaving it alone");
            continue;
        }
        updated += 1;
        tracing::debug!(album_id, %hash, "refreshed album cover from folder sidecar");
    }

    tx.commit().await?;
    Ok(updated)
}

#[cfg(test)]
mod folder_cover_tests {
    use super::*;
    use sqlx::SqlitePool;
    use std::fs;

    /// Minimal slice of the profile schema — only the columns
    /// `refresh_folder_covers` actually reads or writes. Kept hand-rolled
    /// (rather than running the real migrations) because those live in
    /// the app crate, out of reach from `waveflow-core`.
    async fn fixture_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE artwork (
                 id INTEGER PRIMARY KEY,
                 hash TEXT NOT NULL UNIQUE,
                 format TEXT NOT NULL,
                 source TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE album (
                 id INTEGER PRIMARY KEY,
                 artwork_id INTEGER
             );
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 album_id INTEGER,
                 folder_id INTEGER NOT NULL,
                 file_path TEXT NOT NULL,
                 is_available INTEGER NOT NULL DEFAULT 1
             );",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    /// Content is irrelevant — only its blake3 matters, and two different
    /// byte strings give two different hashes.
    fn write_cover(dir: &Path, bytes: &[u8]) {
        fs::write(dir.join("cover.jpg"), bytes).unwrap();
    }

    async fn seed_album(pool: &SqlitePool, album_id: i64, dir: &Path, artwork_id: Option<i64>) {
        sqlx::query("INSERT INTO album (id, artwork_id) VALUES (?, ?)")
            .bind(album_id)
            .bind(artwork_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track (album_id, folder_id, file_path, is_available)
             VALUES (?, 1, ?, 1)",
        )
        .bind(album_id)
        .bind(dir.join("01.flac").to_string_lossy().to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_artwork(pool: &SqlitePool, id: i64, hash: &str, source: &str) {
        sqlx::query(
            "INSERT INTO artwork (id, hash, format, source, created_at)
             VALUES (?, ?, 'jpg', ?, 0)",
        )
        .bind(id)
        .bind(hash)
        .bind(source)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn album_artwork_hash(pool: &SqlitePool, album_id: i64) -> Option<String> {
        sqlx::query_scalar(
            "SELECT aw.hash FROM album al
               JOIN artwork aw ON aw.id = al.artwork_id
              WHERE al.id = ?",
        )
        .bind(album_id)
        .fetch_optional(pool)
        .await
        .unwrap()
    }

    /// The regression from issue #366: the audio files are untouched, so
    /// the scanner's `(mtime, size)` fast path skips them — but the
    /// sidecar next to them was replaced and the album must follow.
    #[tokio::test]
    async fn replacing_a_sidecar_updates_the_album_cover() {
        let music = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        let pool = fixture_pool().await;

        write_cover(music.path(), b"original cover bytes");
        let old_hash = blake3::hash(b"original cover bytes").to_hex().to_string();
        seed_artwork(&pool, 1, &old_hash, "folder").await;
        seed_album(&pool, 10, music.path(), Some(1)).await;

        // User swaps the picture; the audio files are not touched.
        write_cover(music.path(), b"a completely different cover");
        let new_hash = blake3::hash(b"a completely different cover")
            .to_hex()
            .to_string();

        let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

        assert_eq!(updated, 1);
        assert_eq!(album_artwork_hash(&pool, 10).await, Some(new_hash));
    }

    /// An album that already matches its sidecar must not be rewritten —
    /// otherwise every scan would churn `album.artwork_id` library-wide.
    #[tokio::test]
    async fn an_unchanged_sidecar_is_a_no_op() {
        let music = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        let pool = fixture_pool().await;

        write_cover(music.path(), b"steady cover");
        let hash = blake3::hash(b"steady cover").to_hex().to_string();
        seed_artwork(&pool, 1, &hash, "folder").await;
        seed_album(&pool, 10, music.path(), Some(1)).await;

        let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

        assert_eq!(updated, 0);
        assert_eq!(album_artwork_hash(&pool, 10).await, Some(hash));
    }

    /// Embedded artwork outranks a sidecar during extraction, so this
    /// pass must not invert that precedence behind the user's back.
    #[tokio::test]
    async fn embedded_artwork_is_never_overridden_by_a_sidecar() {
        let music = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        let pool = fixture_pool().await;

        let embedded_hash = blake3::hash(b"from the tag").to_hex().to_string();
        seed_artwork(&pool, 1, &embedded_hash, "embedded").await;
        seed_album(&pool, 10, music.path(), Some(1)).await;
        write_cover(music.path(), b"sidecar that must lose");

        let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

        assert_eq!(updated, 0);
        assert_eq!(album_artwork_hash(&pool, 10).await, Some(embedded_hash));
    }

    /// An album with no artwork at all picks the sidecar up.
    #[tokio::test]
    async fn an_album_without_artwork_adopts_the_sidecar() {
        let music = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        let pool = fixture_pool().await;

        seed_album(&pool, 10, music.path(), None).await;
        write_cover(music.path(), b"first ever cover");
        let hash = blake3::hash(b"first ever cover").to_hex().to_string();

        let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

        assert_eq!(updated, 1);
        assert_eq!(album_artwork_hash(&pool, 10).await, Some(hash));
    }

    /// Deleting a sidecar must not blank an album — far more likely to be
    /// a transient state (files being moved) than a request for no cover.
    #[tokio::test]
    async fn a_missing_sidecar_leaves_the_existing_cover_alone() {
        let music = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        let pool = fixture_pool().await;

        let hash = blake3::hash(b"cover that outlives its file")
            .to_hex()
            .to_string();
        seed_artwork(&pool, 1, &hash, "folder").await;
        seed_album(&pool, 10, music.path(), Some(1)).await;
        // No cover.jpg written at all.

        let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

        assert_eq!(updated, 0);
        assert_eq!(album_artwork_hash(&pool, 10).await, Some(hash));
    }

    /// Scoping is by *album touched by the scanned folder*, not by track
    /// folder — a multi-folder album deliberately pulls in its other
    /// directories (see `a_multi_folder_album_resolves_consistently`).
    /// What must stay out is an album with no track in this folder at all.
    #[tokio::test]
    async fn an_album_outside_the_scanned_folder_is_untouched() {
        let music = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        let pool = fixture_pool().await;

        let hash = blake3::hash(b"untouched").to_hex().to_string();
        seed_artwork(&pool, 1, &hash, "folder").await;
        sqlx::query("INSERT INTO album (id, artwork_id) VALUES (10, 1)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track (album_id, folder_id, file_path, is_available)
             VALUES (10, 2, ?, 1)",
        )
        .bind(music.path().join("01.flac").to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
        write_cover(music.path(), b"a new cover in another folder");

        // Scanning folder 1 — the track above lives under folder 2.
        let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

        assert_eq!(updated, 0);
        assert_eq!(album_artwork_hash(&pool, 10).await, Some(hash));
    }

    /// A manually uploaded cover must survive a sidecar sitting next to
    /// the files — the user picked it deliberately. Same for a Deezer
    /// enrichment result below; the guard is an allowlist, so any source
    /// other than `folder` is left alone.
    #[tokio::test]
    async fn a_user_uploaded_cover_is_not_replaced_by_a_sidecar() {
        let music = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        let pool = fixture_pool().await;

        let chosen = blake3::hash(b"the cover the user picked")
            .to_hex()
            .to_string();
        seed_artwork(&pool, 1, &chosen, "user").await;
        seed_album(&pool, 10, music.path(), Some(1)).await;
        write_cover(music.path(), b"a sidecar that must not win");

        let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

        assert_eq!(updated, 0);
        assert_eq!(album_artwork_hash(&pool, 10).await, Some(chosen));
    }

    #[tokio::test]
    async fn a_deezer_cover_is_not_replaced_by_a_sidecar() {
        let music = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        let pool = fixture_pool().await;

        let fetched = blake3::hash(b"fetched from deezer").to_hex().to_string();
        seed_artwork(&pool, 1, &fetched, "deezer").await;
        seed_album(&pool, 10, music.path(), Some(1)).await;
        write_cover(music.path(), b"a sidecar that must not win");

        let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

        assert_eq!(updated, 0);
        assert_eq!(album_artwork_hash(&pool, 10).await, Some(fetched));
    }

    /// An album split across two library folders must resolve to the same
    /// directory whichever folder triggered the scan. Scoping the track
    /// query to the scanned folder would make disc 1 win when folder 1 is
    /// scanned and disc 2 win when folder 2 is, flipping the album's
    /// picture back and forth on every pass.
    #[tokio::test]
    async fn a_multi_folder_album_resolves_consistently() {
        let root = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        // "disc-1" sorts before "disc-2", so disc 1's cover must win in
        // both directions.
        let disc1 = root.path().join("disc-1");
        let disc2 = root.path().join("disc-2");
        fs::create_dir_all(&disc1).unwrap();
        fs::create_dir_all(&disc2).unwrap();
        write_cover(&disc1, b"disc one cover");
        write_cover(&disc2, b"disc two cover");
        let disc1_hash = blake3::hash(b"disc one cover").to_hex().to_string();

        for scanned_folder in [1_i64, 2] {
            let pool = fixture_pool().await;
            sqlx::query("INSERT INTO album (id, artwork_id) VALUES (10, NULL)")
                .execute(&pool)
                .await
                .unwrap();
            for (folder_id, dir) in [(1_i64, &disc1), (2_i64, &disc2)] {
                sqlx::query(
                    "INSERT INTO track (album_id, folder_id, file_path, is_available)
                     VALUES (10, ?, ?, 1)",
                )
                .bind(folder_id)
                .bind(dir.join("01.flac").to_string_lossy().to_string())
                .execute(&pool)
                .await
                .unwrap();
            }

            let updated = refresh_folder_covers(&pool, art.path(), scanned_folder)
                .await
                .unwrap();

            assert_eq!(updated, 1, "scanning folder {scanned_folder}");
            assert_eq!(
                album_artwork_hash(&pool, 10).await,
                Some(disc1_hash.clone()),
                "scanning folder {scanned_folder} must pick the first directory",
            );
        }
    }

    /// The write-time half of the allowlist. `refresh_folder_covers`
    /// filters candidates from a read taken before the blocking directory
    /// walk, so these cases cover what happens when the album's artwork
    /// changes during that window — a race a full-pass test cannot stage
    /// deterministically, which is why the guard is exercised directly.
    ///
    /// Each case releases its connection before reading back through the
    /// pool. Not strictly required today — sqlx shares one in-memory
    /// database across a pool's connections — but the assertions should
    /// not quietly depend on that.
    #[tokio::test]
    async fn the_write_guard_replaces_a_sidecar_cover() {
        let pool = fixture_pool().await;
        seed_artwork(&pool, 1, "old-sidecar", "folder").await;
        seed_artwork(&pool, 2, "new-sidecar", "folder").await;
        sqlx::query("INSERT INTO album (id, artwork_id) VALUES (10, 1)")
            .execute(&pool)
            .await
            .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let changed = link_folder_cover_if_eligible(&mut conn, 10, 2)
            .await
            .unwrap();
        drop(conn);

        assert!(changed);
        assert_eq!(
            album_artwork_hash(&pool, 10).await,
            Some("new-sidecar".to_string())
        );
    }

    #[tokio::test]
    async fn the_write_guard_adopts_a_cover_when_the_album_has_none() {
        let pool = fixture_pool().await;
        seed_artwork(&pool, 2, "new-sidecar", "folder").await;
        sqlx::query("INSERT INTO album (id, artwork_id) VALUES (10, NULL)")
            .execute(&pool)
            .await
            .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let changed = link_folder_cover_if_eligible(&mut conn, 10, 2)
            .await
            .unwrap();
        drop(conn);

        assert!(changed);
        assert_eq!(
            album_artwork_hash(&pool, 10).await,
            Some("new-sidecar".to_string())
        );
    }

    /// The race this guard exists for: the user uploaded a cover after the
    /// candidate list was built. The update must match no row, and must
    /// report that it changed nothing so the summary stays honest.
    #[tokio::test]
    async fn the_write_guard_refuses_a_cover_that_became_user_owned() {
        for source in ["user", "deezer", "embedded"] {
            let pool = fixture_pool().await;
            seed_artwork(&pool, 1, "chosen-mid-scan", source).await;
            seed_artwork(&pool, 2, "new-sidecar", "folder").await;
            sqlx::query("INSERT INTO album (id, artwork_id) VALUES (10, 1)")
                .execute(&pool)
                .await
                .unwrap();

            let mut conn = pool.acquire().await.unwrap();
            let changed = link_folder_cover_if_eligible(&mut conn, 10, 2)
                .await
                .unwrap();
            drop(conn);

            assert!(!changed, "source {source} must not be replaced");
            assert_eq!(
                album_artwork_hash(&pool, 10).await,
                Some("chosen-mid-scan".to_string()),
                "source {source} must survive",
            );
        }
    }
}
