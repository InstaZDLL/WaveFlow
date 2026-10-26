//! Bringing the database in line with an edit: the track, its album,
//! artists and genres, and the respelling of a shared name.

use sqlx::SqlitePool;

use waveflow_core::scanner::extra_tags::{stored_value, COMPOSER_KEY};
use waveflow_core::scanner::{
    canonical_name, split_artist_name, split_genres, upsert_album, upsert_artist, upsert_genre,
};

use crate::error::AppResult;

use super::*;

/// Mirror the file write into the database. Order matters because
/// `track.album_id` depends on the resolved `primary_artist` (album
/// is keyed on `(canonical_title, artist_id)`), and `track_artist`
/// needs the new track row state.
pub(super) async fn sync_db(pool: &SqlitePool, track_id: i64, edit: &TrackEdit) -> AppResult<()> {
    let mut tx = pool.begin().await?;

    // The album the track leaves, if it does: its year may have come
    // from this track.
    let old_album_id: Option<i64> = sqlx::query_scalar("SELECT album_id FROM track WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();

    // Resolve the new artist list (and the new primary). When the
    // user didn't touch the artist field, leave the existing rows
    // alone — re-upserting the same names would no-op but the DELETE
    // + INSERT churn isn't free on a hot library.
    let (artist_ids, primary_artist_id): (Option<Vec<i64>>, Option<i64>) =
        if let Some(raw) = edit.artist.as_ref() {
            let mut ids: Vec<i64> = Vec::new();
            for name in split_artist_name(raw) {
                if let Some(id) = upsert_artist(&mut tx, &name).await? {
                    // First spelling wins when one credit lists an artist twice.
                    if !ids.contains(&id) {
                        respell(&mut tx, Respell::Artist, id, &name).await?;
                        ids.push(id);
                    }
                }
            }
            let primary = ids.first().copied();
            (Some(ids), primary)
        } else {
            (None, None)
        };

    // Resolve album similarly. Needs the (possibly new) primary
    // artist; falls back to the existing one when the user didn't
    // edit the artist field.
    //
    // The album the track is in now: its title, Album Artist and
    // compilation flag, which a relink carries over unless the edit
    // replaces them.
    let current_album: Option<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT al.title, al.album_artist, al.is_compilation
           FROM track t
           JOIN album al ON al.id = t.album_id
          WHERE t.id = ?",
    )
    .bind(track_id)
    .fetch_optional(&mut *tx)
    .await?;
    // An edited Album Artist relinks the track even with the title left
    // alone: albums are keyed on `(canonical_title, album artist)`, so a
    // new album artist is a different album — the one a rescan of the
    // file it now tags would put it in. The title is then the current
    // album's; a track in no album has nothing to regroup, and the
    // value lives in the file alone.
    let album_title: Option<String> = match edit.album.as_ref() {
        Some(title) => Some(title.trim().to_string()),
        None if edit.album_artist.is_some() => {
            current_album.as_ref().map(|(title, _, _)| title.clone())
        }
        None => None,
    };
    let new_album_id: Option<Option<i64>> = if let Some(title) = album_title {
        if title.is_empty() {
            Some(None)
        } else {
            // Use the new primary artist when the user changed it,
            // otherwise read the current one back so the album
            // dedup still keys on a stable artist.
            let aid = match primary_artist_id {
                Some(_) => primary_artist_id,
                None => {
                    sqlx::query_scalar::<_, Option<i64>>(
                        "SELECT primary_artist FROM track WHERE id = ?",
                    )
                    .bind(track_id)
                    .fetch_one(&mut *tx)
                    .await?
                }
            };
            // Preserve the existing album's album_artist + compilation
            // flag when the user renames an album without re-tagging
            // the source file. Without this, a rename routed through
            // upsert_album would fall back to the track's primary
            // artist for grouping, re-introducing the v1.0 split-on-
            // featuring bug for the renamed row.
            //
            // An Album Artist the edit names replaces the carried one,
            // and an emptied one leaves the grouping to the same
            // fallback the scanner uses: the compilation flag, then the
            // track's first artist.
            //
            // The flag is carried only when the edit names no Album
            // Artist. It may be the left album's own — a TCMP, or a
            // `merge_implicit_compilations` verdict on tracks that had no
            // Album Artist — and is sticky wherever it lands, so carried
            // onto "Queen" it would make (greatest hits, Queen) a
            // compilation for good: a state no scan of the retagged files
            // builds, and one the scanner's "existing compilation of this
            // title" fallback would then hand other artists' tracks. A
            // file that does carry TCMP gets it back on its next scan.
            let (carried_album_artist, carried_is_compilation) = current_album
                .as_ref()
                .map(|(_, aa, cmp)| (aa.clone(), *cmp == 1))
                .unwrap_or((None, false));
            let named_album_artist = edit
                .album_artist
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let (album_artist, is_compilation) = match edit.album_artist.as_ref() {
                Some(_) => (
                    named_album_artist,
                    carried_is_compilation && named_album_artist.is_none(),
                ),
                None => (carried_album_artist.as_deref(), carried_is_compilation),
            };
            // upsert_album now takes &mut SqliteConnection, so we
            // can call it directly inside the open transaction —
            // no commit/reopen dance needed.
            let aid = upsert_album(
                &mut tx,
                &title,
                album_artist,
                is_compilation,
                aid,
                edit.year,
            )
            .await?;
            if let Some(id) = aid {
                if edit.album.is_some() {
                    respell(&mut tx, Respell::Album, id, &title).await?;
                }
                // The spelling typed, on the album and on its artist —
                // the respell every other edited name gets. The album's
                // own `COALESCE` keeps the first spelling it saw, so
                // without this "the beatles" typed over "The Beatles"
                // reached the file and nothing on screen.
                if let Some(name) = named_album_artist {
                    sqlx::query("UPDATE album SET album_artist = ? WHERE id = ?")
                        .bind(name)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    let artist_id: Option<i64> =
                        sqlx::query_scalar("SELECT artist_id FROM album WHERE id = ?")
                            .bind(id)
                            .fetch_one(&mut *tx)
                            .await?;
                    if let Some(artist_id) = artist_id {
                        respell(&mut tx, Respell::Artist, artist_id, name).await?;
                    }
                }
            }
            Some(aid)
        }
    } else {
        None
    };

    // Patch the track row. We build the SET clause dynamically so
    // unset fields keep their current values instead of getting
    // clobbered to NULL.
    let mut sets: Vec<&str> = Vec::new();
    if edit.title.is_some() {
        sets.push("title = ?");
        // A renamed title has a different romanisation, and the backfill
        // only ever looks at NULLs -- so a stale blob would stay stale
        // for good if it were not rewritten here (#579).
        sets.push("pinyin = ?");
    }
    if edit.year.is_some() {
        sets.push("year = ?");
    }
    if edit.track_number.is_some() {
        sets.push("track_number = ?");
    }
    if edit.disc_number.is_some() {
        sets.push("disc_number = ?");
    }
    if let Some(pid) = primary_artist_id {
        let _ = pid; // keep variable readable in the binding loop below
        sets.push("primary_artist = ?");
    }
    if new_album_id.is_some() {
        sets.push("album_id = ?");
    }
    // Bumping a `last_modified_ms`-style column would be ideal but
    // the schema doesn't carry one yet — skipping for v1.

    if !sets.is_empty() {
        let sql = format!("UPDATE track SET {} WHERE id = ?", sets.join(", "));
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        if let Some(t) = edit.title.as_ref() {
            q = q.bind(t.trim());
            q = q.bind(waveflow_core::scanner::pinyin_blob(t.trim()).unwrap_or_default());
        }
        if let Some(y) = edit.year {
            q = q.bind(if y > 0 { Some(y) } else { None });
        }
        if let Some(n) = edit.track_number {
            q = q.bind(if n > 0 { Some(n) } else { None });
        }
        if let Some(n) = edit.disc_number {
            q = q.bind(if n > 0 { Some(n) } else { None });
        }
        if let Some(pid) = primary_artist_id {
            q = q.bind(pid);
        }
        if let Some(aid) = new_album_id {
            q = q.bind(aid);
        }
        q = q.bind(track_id);
        q.execute(&mut *tx).await?;
    }

    // A track moved by its Album Artist alone into an album this edit
    // created takes the cover of the one it left: same title, same
    // record. The new row starts without artwork, and the file's embedded
    // picture only reaches it on the next scan — so fixing the Album
    // Artist of a whole record left it coverless until then. Only into an
    // album holding no other track and no cover of its own: an existing
    // album keeps what it has. Never on a rename, where the album left is
    // another record — its cover, and a `manual` source no scan
    // overrides, would stick to the wrong album.
    let moved_by_album_artist = edit.album.is_none() && edit.album_artist.is_some();
    if let (true, Some(Some(new_id)), Some(old_id)) =
        (moved_by_album_artist, new_album_id, old_album_id)
    {
        if new_id != old_id {
            sqlx::query(
                "UPDATE album
                    SET artwork_id = (SELECT artwork_id FROM album WHERE id = ?1),
                        artwork_source = (SELECT artwork_source FROM album WHERE id = ?1)
                  WHERE id = ?2
                    AND artwork_id IS NULL
                    AND NOT EXISTS (SELECT 1 FROM track WHERE album_id = ?2 AND id <> ?3)",
            )
            .bind(old_id)
            .bind(new_id)
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        }
    }

    // Replace track_artist links when the user touched the artist
    // field. Empty list → no rows (the track stays available, just
    // with no linked artists which the UI will render as "—").
    if let Some(ids) = artist_ids {
        sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        for (pos, aid) in ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO track_artist (track_id, artist_id, position, role)
                 VALUES (?, ?, ?, 'main')",
            )
            .bind(track_id)
            .bind(aid)
            .bind(pos as i64)
            .execute(&mut *tx)
            .await?;
        }
    }

    // Genres: every one the edit names, split as the file was written so
    // the next scan finds the same set (#729).
    if let Some(g) = edit.genre.as_ref() {
        sqlx::query("DELETE FROM track_genre WHERE track_id = ?")
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        for name in split_genres([g.as_str()]) {
            if let Some(gid) = upsert_genre(&mut tx, &name).await? {
                respell(&mut tx, Respell::Genre, gid, &name).await?;
                sqlx::query("INSERT OR IGNORE INTO track_genre (track_id, genre_id) VALUES (?, ?)")
                    .bind(track_id)
                    .bind(gid)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }

    // The composer lives beside the track, in the row the scanner fills
    // from the same frame, under the same key and cut to the same length
    // — so the next scan of the file finds what this wrote.
    if let Some(c) = edit.composer.as_ref() {
        sqlx::query("DELETE FROM track_tag WHERE track_id = ? AND key = ?")
            .bind(track_id)
            .bind(COMPOSER_KEY)
            .execute(&mut *tx)
            .await?;
        if let Some(value) = stored_value(c) {
            sqlx::query("INSERT INTO track_tag (track_id, key, value) VALUES (?, ?, ?)")
                .bind(track_id)
                .bind(COMPOSER_KEY)
                .bind(value)
                .execute(&mut *tx)
                .await?;
        }
    }

    // The album year is the first track's (disc, then track order),
    // recomputed by the scan only: an edited year, a renumbered track, or
    // a track moved between albums left the header on the old one until
    // the next scan of that folder.
    if edit.year.is_some()
        || edit.track_number.is_some()
        || edit.disc_number.is_some()
        || new_album_id.is_some()
    {
        let mut albums: Vec<i64> = old_album_id.into_iter().collect();
        if let Some(Some(aid)) = new_album_id {
            albums.push(aid);
        }
        waveflow_core::repository::sqlite::album::refresh_album_years(&mut tx, &albums).await?;
    }

    tx.commit().await?;
    // Suppress unused-import warning when canonical_name isn't reached
    // via a code path in this function (it's exported for callers).
    let _ = canonical_name;
    Ok(())
}

/// Which entity an edit names, for [`respell`].
#[derive(Clone, Copy)]
enum Respell {
    Artist,
    Album,
    Genre,
}

/// Give an artist, album or genre the spelling the user just typed.
///
/// They are found by a canonical key that ignores case, punctuation and
/// spacing, so "The Wall" typed over "the wall" resolves to the very same
/// row — and nothing then renamed it: the file carried the fix, every
/// view kept the old spelling, and no rescan could change it either.
///
/// Only here, on an explicit edit. The scanner must not do it: two files
/// spelling one artist differently would rename it back and forth on
/// every scan. It renames the entity for every track that shares it,
/// which is what correcting a name means. Pinyin travels with the name.
async fn respell(
    conn: &mut sqlx::SqliteConnection,
    kind: Respell,
    id: i64,
    spelled: &str,
) -> AppResult<()> {
    let pinyin = waveflow_core::scanner::pinyin_blob(spelled).unwrap_or_default();
    let query = match kind {
        Respell::Artist => {
            sqlx::query("UPDATE artist SET name = ?, pinyin = ? WHERE id = ? AND name IS NOT ?")
                .bind(spelled)
                .bind(pinyin)
                .bind(id)
                .bind(spelled)
        }
        Respell::Album => {
            sqlx::query("UPDATE album SET title = ?, pinyin = ? WHERE id = ? AND title IS NOT ?")
                .bind(spelled)
                .bind(pinyin)
                .bind(id)
                .bind(spelled)
        }
        Respell::Genre => sqlx::query("UPDATE genre SET name = ? WHERE id = ? AND name IS NOT ?")
            .bind(spelled)
            .bind(id)
            .bind(spelled),
    };
    query.execute(&mut *conn).await?;
    Ok(())
}
