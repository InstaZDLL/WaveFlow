//! Album-artist passes: the post-scan `merge_implicit_compilations`
//! promotion and the per-track repoint of a fallback album artist.

use sqlx::SqlitePool;

use crate::error::CoreResult;

use super::{upsert_artist, VARIOUS_ARTISTS_LABEL};

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
            // The record lives on under the survivor's id; so does a
            // sidebar pin, which the delete below would cascade away.
            crate::repository::sqlite::sidebar_pin::carry_album_pin(&mut tx, *sid, survivor)
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

/// Follow a track's re-split artist credits onto its album, when the album
/// took its artist from that track in the first place.
///
/// An album with no Album Artist tag and no compilation flag is keyed on
/// its first track's primary artist (see
/// [`resolve_album_artist`](super::resolve_album_artist)). When a rescan
/// splits that track's credits anew, the album should follow the new lead
/// artist — but only then:
///
/// - `album_artist IS NULL AND is_compilation = 0`: an explicit album
///   artist or a Various Artists record is the tag's decision, and a
///   track's credits must never override it.
/// - `artist_id IS previous_primary`: the album was keyed on *this*
///   track's old lead. An album keyed on another track's artist is not
///   this track's to move. `IS`, not `=`, so a NULL on both sides matches.
///
/// When another album already holds `(canonical_title, new_primary)` the
/// album cannot take that key — the pair is `UNIQUE`, and the collision
/// would abort the whole scan transaction. The track joins that album
/// instead, which is where a fresh scan would have put it; the album it
/// leaves drops out of the lists once its last track has followed.
///
/// Returns whether the track ended up under the new artist.
pub async fn repoint_fallback_album_artist(
    conn: &mut sqlx::SqliteConnection,
    track_id: i64,
    previous_primary: Option<i64>,
    new_primary: i64,
) -> CoreResult<bool> {
    let eligible: Option<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT cur.id,
                (SELECT other.id FROM album other
                  WHERE other.canonical_title = cur.canonical_title
                    AND other.artist_id = ?1
                  LIMIT 1)
           FROM album cur
          WHERE cur.id = (SELECT album_id FROM track WHERE id = ?2)
            AND cur.album_artist IS NULL
            AND cur.is_compilation = 0
            AND cur.artist_id IS ?3
            AND cur.artist_id IS NOT ?1",
    )
    .bind(new_primary)
    .bind(track_id)
    .bind(previous_primary)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((album_id, existing)) = eligible else {
        return Ok(false);
    };
    match existing {
        Some(target) => {
            sqlx::query("UPDATE track SET album_id = ? WHERE id = ?")
                .bind(target)
                .bind(track_id)
                .execute(&mut *conn)
                .await?;
        }
        None => {
            sqlx::query("UPDATE album SET artist_id = ? WHERE id = ?")
                .bind(new_primary)
                .bind(album_id)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
