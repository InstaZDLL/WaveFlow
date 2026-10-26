//! Placeholders: the entries of an imported playlist the library does not
//! hold (yet). See [`crate::playlist_import`] for what they are for, and the
//! migration `20261027120000_playlist_placeholder.sql` for the layout.
//!
//! A placeholder's `position` is in `playlist_track.position`'s numbering:
//! it sits just before the real track holding the same position, or at the
//! tail when none does; `ordinal` orders the placeholders of one gap. So
//! every write that moves real tracks moves these the same way —
//! [`shift_after_removal`] and [`shift_for_move`] are called by the
//! `playlist_track` helpers in [`super::playlist`] inside their own
//! transaction — and an append needs nothing: it lands at the tail, after
//! the placeholders already there, because a placeholder sorts before the
//! real track it shares a position with.

use serde::Serialize;
use sqlx::{FromRow, SqliteConnection, SqlitePool};

use crate::error::{CoreError, CoreResult};
use crate::playlist_import::matching::{Candidate, LibraryIndex, Wanted};
use crate::playlist_import::{normalize_isrc, path_key, ImportEntry, ImportSource};

/// One placeholder, as the playlist view lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, FromRow)]
pub struct Placeholder {
    pub id: i64,
    pub playlist_id: i64,
    pub position: i64,
    pub ordinal: i64,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
    pub isrc: Option<String>,
    pub locator: Option<String>,
    pub source: String,
    pub added_at: i64,
    /// The available track the placeholder is shown before, `None` at the
    /// tail. The view lists only available tracks, so it places a
    /// placeholder by this rather than by counting positions, which an
    /// unplugged drive leaves holes in. Filled by [`list`] only.
    #[sqlx(default)]
    pub before_track_id: Option<i64>,
}

/// An entry of an import, after the matcher looked for it.
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    pub entry: ImportEntry,
    pub track_id: Option<i64>,
}

/// What an import wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ImportCounts {
    /// Real tracks added.
    pub imported: u32,
    /// Entries kept as placeholders.
    pub placeholders: u32,
    /// Entries the playlist already held a track for — a playlist holds a
    /// track once (`playlist_track`'s key), so a repeat is dropped.
    pub duplicates: u32,
}

/// Write an import's entries into `playlist_id`, in order, after whatever
/// it already holds. Real tracks take the next positions; a placeholder
/// takes the position of the real track that will follow it.
pub async fn write_entries(
    conn: &mut SqliteConnection,
    playlist_id: i64,
    entries: &[ResolvedEntry],
    source: ImportSource,
    now_ms: i64,
) -> CoreResult<ImportCounts> {
    let tail: Option<i64> =
        sqlx::query_scalar("SELECT MAX(position) FROM playlist_track WHERE playlist_id = ?")
            .bind(playlist_id)
            .fetch_one(&mut *conn)
            .await?;
    let mut next_position = tail.map(|p| p + 1).unwrap_or(0);
    // Placeholders already at the tail keep their place ahead of the new
    // ones in the same gap.
    let mut next_ordinal: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM playlist_placeholder
          WHERE playlist_id = ? AND position >= ?",
    )
    .bind(playlist_id)
    .bind(next_position)
    .fetch_one(&mut *conn)
    .await?;

    let mut counts = ImportCounts::default();
    for resolved in entries {
        match resolved.track_id {
            Some(track_id) => {
                let inserted = sqlx::query(
                    "INSERT OR IGNORE INTO playlist_track
                         (playlist_id, track_id, position, added_at)
                     VALUES (?, ?, ?, ?)",
                )
                .bind(playlist_id)
                .bind(track_id)
                .bind(next_position)
                .bind(now_ms)
                .execute(&mut *conn)
                .await?;
                if inserted.rows_affected() > 0 {
                    counts.imported += 1;
                    next_position += 1;
                    next_ordinal = 0;
                } else {
                    counts.duplicates += 1;
                }
            }
            None => {
                let entry = &resolved.entry;
                sqlx::query(
                    "INSERT INTO playlist_placeholder
                         (playlist_id, position, ordinal, title, artist, album,
                          duration_ms, isrc, locator, source, added_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(playlist_id)
                .bind(next_position)
                .bind(next_ordinal)
                .bind(&entry.title)
                .bind(entry.artist.as_deref())
                .bind(entry.album.as_deref())
                .bind(entry.duration_ms)
                .bind(entry.isrc.as_deref())
                .bind(entry.locator.as_deref())
                .bind(source.as_str())
                .bind(now_ms)
                .execute(&mut *conn)
                .await?;
                counts.placeholders += 1;
                next_ordinal += 1;
            }
        }
    }
    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now_ms)
        .bind(playlist_id)
        .execute(conn)
        .await?;
    Ok(counts)
}

/// Every placeholder of a playlist, in its order.
pub async fn list(pool: &SqlitePool, playlist_id: i64) -> CoreResult<Vec<Placeholder>> {
    Ok(sqlx::query_as::<_, Placeholder>(
        "SELECT ph.id, ph.playlist_id, ph.position, ph.ordinal, ph.title, ph.artist,
                ph.album, ph.duration_ms, ph.isrc, ph.locator, ph.source, ph.added_at,
                (SELECT pt.track_id FROM playlist_track pt
                   JOIN track t ON t.id = pt.track_id
                  WHERE pt.playlist_id = ph.playlist_id
                    AND pt.position >= ph.position
                    AND t.is_available = 1
                  ORDER BY pt.position
                  LIMIT 1) AS before_track_id
           FROM playlist_placeholder ph
          WHERE ph.playlist_id = ?
          ORDER BY ph.position, ph.ordinal, ph.id",
    )
    .bind(playlist_id)
    .fetch_all(pool)
    .await?)
}

/// Remove one placeholder. Returns its playlist, or `None` when there was
/// no such row. The gap it leaves needs no renumbering: placeholders are
/// ordered within their gap, never counted.
pub async fn delete(conn: &mut SqliteConnection, id: i64) -> CoreResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("DELETE FROM playlist_placeholder WHERE id = ? RETURNING playlist_id")
            .bind(id)
            .fetch_optional(conn)
            .await?,
    )
}

/// Renumber the placeholders at `later` so that they follow those at
/// `earlier` once both gaps are one: two gaps meet whenever the track
/// between them goes, and each one's ordinals start at 0.
async fn append_gap(
    conn: &mut SqliteConnection,
    playlist_id: i64,
    earlier: i64,
    later: i64,
) -> CoreResult<()> {
    sqlx::query(
        "UPDATE playlist_placeholder
            SET ordinal = ordinal + (SELECT COALESCE(MAX(ordinal) + 1, 0)
                                       FROM playlist_placeholder
                                      WHERE playlist_id = ?1 AND position = ?2)
          WHERE playlist_id = ?1 AND position = ?3",
    )
    .bind(playlist_id)
    .bind(earlier)
    .bind(later)
    .execute(conn)
    .await?;
    Ok(())
}

/// The real track at `position` left the playlist and the ones after it
/// moved up one: the placeholders after it move with them. A placeholder
/// at `position` itself stays — it now sits before the track that took
/// the place, followed by the gap that came after the track that left.
pub async fn shift_after_removal(
    conn: &mut SqliteConnection,
    playlist_id: i64,
    position: i64,
) -> CoreResult<()> {
    append_gap(conn, playlist_id, position, position + 1).await?;
    sqlx::query(
        "UPDATE playlist_placeholder SET position = position - 1
          WHERE playlist_id = ? AND position > ?",
    )
    .bind(playlist_id)
    .bind(position)
    .execute(conn)
    .await?;
    Ok(())
}

/// A real track moved from `from` to `to`, and the tracks in between
/// shifted one place toward `from`. The placeholders among those tracks
/// shift with them, so each still sits before the track it sat before;
/// the moved track jumps over them. The gap it leaves behind — the one
/// before it and the one after it — becomes one, in their order.
pub async fn shift_for_move(
    conn: &mut SqliteConnection,
    playlist_id: i64,
    from: i64,
    to: i64,
) -> CoreResult<()> {
    if to > from {
        // The gap after the moved track comes down onto the one before it.
        append_gap(conn, playlist_id, from, from + 1).await?;
        sqlx::query(
            "UPDATE playlist_placeholder SET position = position - 1
              WHERE playlist_id = ? AND position > ? AND position <= ?",
        )
        .bind(playlist_id)
        .bind(from)
        .bind(to)
        .execute(conn)
        .await?;
    } else if to < from {
        // The gap before the moved track goes up onto the one after it,
        // ahead of it.
        append_gap(conn, playlist_id, from, from + 1).await?;
        sqlx::query(
            "UPDATE playlist_placeholder SET position = position + 1
              WHERE playlist_id = ? AND position >= ? AND position <= ?",
        )
        .bind(playlist_id)
        .bind(to)
        .bind(from)
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// Put `track_id` in the place of placeholder `id`, and remove the
/// placeholder. The real tracks from its position on move down one, and
/// so do the placeholders after it — those of its own gap that came after
/// it included, which now follow the track instead of preceding it.
///
/// A playlist holds a track once: when this one already holds it, the
/// placeholder only goes. Returns whether a track was added, or `None`
/// when there was no such placeholder.
pub async fn fill(
    conn: &mut SqliteConnection,
    id: i64,
    track_id: i64,
    now_ms: i64,
) -> CoreResult<Option<bool>> {
    let Some((playlist_id, position, ordinal)) = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT playlist_id, position, ordinal FROM playlist_placeholder WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    sqlx::query("DELETE FROM playlist_placeholder WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    let held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM playlist_track WHERE playlist_id = ? AND track_id = ?)",
    )
    .bind(playlist_id)
    .bind(track_id)
    .fetch_one(&mut *conn)
    .await?;
    if held {
        // The placeholder went all the same: stamp the playlist, which is
        // what the view keys its refetch on.
        sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
            .bind(now_ms)
            .bind(playlist_id)
            .execute(conn)
            .await?;
        return Ok(Some(false));
    }

    sqlx::query(
        "UPDATE playlist_track SET position = position + 1
          WHERE playlist_id = ? AND position >= ?",
    )
    .bind(playlist_id)
    .bind(position)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE playlist_placeholder SET position = position + 1
          WHERE playlist_id = ? AND (position > ? OR (position = ? AND ordinal > ?))",
    )
    .bind(playlist_id)
    .bind(position)
    .bind(position)
    .bind(ordinal)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO playlist_track (playlist_id, track_id, position, added_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(playlist_id)
    .bind(track_id)
    .bind(position)
    .bind(now_ms)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now_ms)
        .bind(playlist_id)
        .execute(conn)
        .await?;
    Ok(Some(true))
}

#[derive(FromRow)]
struct CandidateRow {
    id: i64,
    title: String,
    duration_ms: i64,
    file_path: String,
    artists: Option<String>,
    isrc: Option<String>,
}

/// Separator for the artist names packed into one column below: the unit
/// separator, which no tag contains.
const NAME_SEP: char = '\u{1f}';

/// Every available track, as the matcher sees it, its path not yet made
/// comparable — that is CPU work for the caller's blocking pool
/// ([`index_paths`]), and only worth doing when an entry names a path.
pub async fn load_candidates(pool: &SqlitePool) -> CoreResult<Vec<Candidate>> {
    let rows = sqlx::query_as::<_, CandidateRow>(
        "SELECT t.id, t.title, t.duration_ms, t.file_path,
                (SELECT GROUP_CONCAT(name, char(31)) FROM (
                    SELECT ar.name FROM track_artist ta
                      JOIN artist ar ON ar.id = ta.artist_id
                     WHERE ta.track_id = t.id
                     ORDER BY ta.position
                )) AS artists,
                (SELECT tt.value FROM track_tag tt
                  WHERE tt.track_id = t.id AND tt.key = 'ISRC' COLLATE NOCASE
                  LIMIT 1) AS isrc
           FROM track t
          WHERE t.is_available = 1",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Candidate {
            track_id: row.id,
            title: row.title,
            artists: row
                .artists
                .map(|names| names.split(NAME_SEP).map(str::to_string).collect())
                .unwrap_or_default(),
            duration_ms: row.duration_ms,
            isrc: row.isrc.as_deref().and_then(normalize_isrc),
            file_path: row.file_path,
            path_key: None,
            file_name: None,
        })
        .collect())
}

/// Give every candidate the comparable form of its path ([`path_key`],
/// lexical — no filesystem access) and its file name.
pub fn index_paths(candidates: &mut [Candidate]) {
    for candidate in candidates {
        let (key, name) = locator_keys(&candidate.file_path);
        candidate.path_key = Some(key);
        candidate.file_name = name;
    }
}

/// The keys a placeholder (or an M3U entry) gives the matcher for the path
/// it names: its comparable form and its file name.
pub fn locator_keys(locator: &str) -> (String, Option<String>) {
    (
        path_key(std::path::Path::new(locator)),
        crate::playlist_import::file_name(locator).map(str::to_lowercase),
    )
}

/// Give every placeholder whose track the library now holds that track.
/// Run after a scan, so a track that arrives — rescanned, dropped in,
/// picked up by the watcher — takes its place in every playlist waiting
/// for it. A placeholder whose playlist already holds the track is
/// dropped instead. Returns the playlists that changed, each once, for the
/// caller to announce.
///
/// One transaction for the whole pass, after a cheap probe: a profile
/// with no placeholder, which is nearly every profile, pays one query.
pub async fn fill_pending(pool: &SqlitePool, now_ms: i64) -> CoreResult<Vec<i64>> {
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist_placeholder")
        .fetch_one(pool)
        .await?;
    if pending == 0 {
        return Ok(Vec::new());
    }
    let placeholders = sqlx::query_as::<_, Placeholder>(
        "SELECT id, playlist_id, position, ordinal, title, artist, album, duration_ms,
                isrc, locator, source, added_at
           FROM playlist_placeholder
          ORDER BY playlist_id, position, ordinal, id",
    )
    .fetch_all(pool)
    .await?;
    let mut candidates = load_candidates(pool).await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    // Indexing the library and matching is CPU work, and this runs after
    // every scan — a watcher's one-file rescan included: off the runtime.
    let matches: Vec<(i64, i64, i64)> = tokio::task::spawn_blocking(move || {
        if placeholders.iter().any(|p| p.locator.is_some()) {
            index_paths(&mut candidates);
        }
        let index = LibraryIndex::new(candidates);
        placeholders
            .iter()
            .filter_map(|placeholder| {
                let keys = placeholder.locator.as_deref().map(locator_keys);
                let wanted = Wanted {
                    title: &placeholder.title,
                    artist: placeholder.artist.as_deref(),
                    duration_ms: placeholder.duration_ms,
                    isrc: placeholder.isrc.as_deref(),
                    path_key: keys.as_ref().map(|(key, _)| key.as_str()),
                    file_name: keys.as_ref().and_then(|(_, name)| name.as_deref()),
                };
                index
                    .find(&wanted)
                    .map(|track_id| (placeholder.id, track_id, placeholder.playlist_id))
            })
            .collect()
    })
    .await
    .map_err(|e| CoreError::Other(format!("placeholder match join: {e}")))?;
    if matches.is_empty() {
        return Ok(Vec::new());
    }

    // `fill` reads each placeholder's position as it is at that moment —
    // filling one shifts the ones after it, so the positions loaded above
    // are stale by the second fill.
    let mut tx = pool.begin().await?;
    let mut changed: Vec<i64> = Vec::new();
    for (id, track_id, playlist_id) in matches {
        if fill(&mut tx, id, track_id, now_ms).await?.is_some() && !changed.contains(&playlist_id) {
            changed.push(playlist_id);
        }
    }
    tx.commit().await?;
    Ok(changed)
}
