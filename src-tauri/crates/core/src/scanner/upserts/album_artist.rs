//! Album-artist passes: the post-scan `merge_implicit_compilations`
//! promotion and the per-track repoint of a fallback album artist.

use std::collections::HashSet;

use sqlx::SqlitePool;

use crate::error::CoreResult;
use crate::scanner::folders::same_directory;

use super::{upsert_artist, VARIOUS_ARTISTS_LABEL};

/// One album row a merge could take: its available tracks, as far as telling
/// a compilation from an unrelated album of the same title needs them.
#[derive(Debug, Clone, PartialEq)]
struct CandidateAlbum {
    id: i64,
    artist_id: Option<i64>,
    year: Option<i64>,
    tracks: Vec<CandidateTrack>,
}

#[derive(Debug, Clone, PartialEq)]
struct CandidateTrack {
    path: String,
    /// `(disc, track)`, a missing disc counting as the first. `None` when the
    /// file has no track number.
    place: Option<(i64, i64)>,
}

/// Albums judged to be one record, and what that judgement rests on.
#[derive(Debug, Default)]
struct Cluster {
    albums: Vec<CandidateAlbum>,
    /// Every place taken so far. Two albums claiming one are two records.
    places: HashSet<(i64, i64)>,
    collides: bool,
}

impl Cluster {
    fn add(&mut self, album: CandidateAlbum) {
        let own: HashSet<(i64, i64)> = album.tracks.iter().filter_map(|t| t.place).collect();
        self.collides |= !self.places.is_disjoint(&own);
        self.places.extend(own);
        self.albums.push(album);
    }

    /// The one year all its albums state, with every track numbered: what it
    /// takes to tie folders together when nothing else does.
    fn numbered_year(&self) -> Option<i64> {
        let year = self.albums.first()?.year?;
        let all = self.albums.iter().all(|album| {
            album.year == Some(year) && album.tracks.iter().all(|t| t.place.is_some())
        });
        all.then_some(year)
    }

    fn distinct_artists(&self) -> usize {
        self.albums
            .iter()
            .filter_map(|a| a.artist_id)
            .collect::<HashSet<_>>()
            .len()
    }
}

/// The records hiding among same-title album rows, each in `id` order.
///
/// Rows are tied together when their tracks share a directory, then when
/// they state the same year with every track numbered, so that a compilation
/// sorted into one folder per artist is still seen. Either way a place on
/// the record (`disc`, `track`) claimed by two rows rules the tie out: a
/// compilation has one track at each number, and three bands' "Greatest Hits"
/// all have a track one, flat in one folder or released the same year.
fn compilations_among(mut albums: Vec<CandidateAlbum>) -> Vec<Vec<CandidateAlbum>> {
    albums.sort_by_key(|album| album.id);

    let mut by_directory: Vec<Cluster> = Vec::new();
    for album in albums {
        let shares = |cluster: &Cluster| {
            cluster.albums.iter().flat_map(|a| &a.tracks).any(|theirs| {
                album
                    .tracks
                    .iter()
                    .any(|ours| same_directory(&theirs.path, &ours.path))
            })
        };
        // Every cluster it touches is one record with it.
        let (touched, rest): (Vec<Cluster>, Vec<Cluster>) =
            by_directory.into_iter().partition(shares);
        by_directory = rest;
        let mut merged = Cluster::default();
        for other in touched.into_iter().flat_map(|cluster| cluster.albums) {
            merged.add(other);
        }
        merged.add(album);
        by_directory.push(merged);
    }
    by_directory.sort_by_key(|cluster| cluster.albums.iter().map(|a| a.id).min());

    let mut records: Vec<Cluster> = Vec::new();
    for cluster in by_directory {
        let year = cluster.numbered_year().filter(|_| !cluster.collides);
        let joined = year.and_then(|year| {
            records.iter_mut().find(|record| {
                !record.collides
                    && record.numbered_year() == Some(year)
                    && record.places.is_disjoint(&cluster.places)
            })
        });
        match joined {
            Some(record) => {
                for album in cluster.albums {
                    record.add(album);
                }
            }
            None => records.push(cluster),
        }
    }

    records
        .into_iter()
        .filter(|record| !record.collides && record.distinct_artists() >= 3)
        .map(|mut record| {
            record.albums.sort_by_key(|album| album.id);
            record.albums
        })
        .collect()
}

/// Post-scan pass that promotes "tagless" same-title album rows into a
/// single Various-Artists compilation. Catches the common case where a
/// lofi / mood / cover-pack compilation (Soothing Breeze, Coffee Shop,
/// etc.) ships without `aART` and without `TCMP` so each track lands
/// in its own primary-artist-keyed album row.
///
/// A title proves nothing on its own, so the heuristic has to be careful with
/// it — legit cases like "two different artists who self-titled", or three
/// bands' "Greatest Hits":
///   - same `canonical_title`
///   - every row has `album_artist IS NULL AND is_compilation = 0`
///     (the tag-driven path is the source of truth — never override)
///   - the rows hold together as one record: see [`compilations_among`]
///   - at least 3 distinct `artist_id`s (so a featuring on one track
///     of a normal album doesn't get promoted to a fake compilation)
///
/// On match: pick the lowest-id row as survivor, set
/// `(artist_id = VariousArtists, album_artist = "Various Artists",
/// is_compilation = 1)`, reparent every track of the sibling rows
/// onto the survivor, and delete the siblings. Their artwork rows
/// stay around (other albums may share them via hash dedup).
///
/// A title holds one Various Artists row (`UNIQUE (canonical_title,
/// artist_id)`). When it already has one, the record joins it only if it
/// lies beside its tracks with no place taken twice, and otherwise stays as
/// it is: fragmented is what a scan of the files gives, and nothing is lost.
///
/// Rows an older build merged on the title alone stay merged. Nothing on such
/// a row tells a guess from a tag, and taking it apart would lose what was
/// since set on it (a cover, a pin, an identifier), so a scan does not try.
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
        type Row = (
            i64,
            Option<i64>,
            Option<i64>,
            String,
            Option<i64>,
            Option<i64>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT al.id, al.artist_id, al.year, t.file_path, t.disc_number, t.track_number
               FROM album al
               JOIN track t ON t.album_id = al.id AND t.is_available = 1
              WHERE al.canonical_title = ?
                AND al.album_artist IS NULL
                AND al.is_compilation = 0
              ORDER BY al.id ASC",
        )
        .bind(&canonical_title)
        .fetch_all(&mut *tx)
        .await?;
        let mut albums: Vec<CandidateAlbum> = Vec::new();
        for (id, artist_id, year, path, disc, track) in rows {
            // Files tagged with the artist "Various Artists" and nothing else
            // make a row under that key with no compilation flag. It is what
            // a merge lands on, never one of the rows it takes.
            if artist_id == Some(va_id) {
                continue;
            }
            let candidate = CandidateTrack {
                path,
                place: track.map(|track| (disc.unwrap_or(1), track)),
            };
            match albums.last_mut().filter(|album| album.id == id) {
                Some(album) => album.tracks.push(candidate),
                None => albums.push(CandidateAlbum {
                    id,
                    artist_id,
                    year,
                    tracks: vec![candidate],
                }),
            }
        }

        for record in compilations_among(albums) {
            let existing: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM album WHERE canonical_title = ? AND artist_id = ?",
            )
            .bind(&canonical_title)
            .bind(va_id)
            .fetch_optional(&mut *tx)
            .await?;
            let (survivor, siblings): (i64, &[CandidateAlbum]) = match existing {
                Some(row) => {
                    if !lies_beside(&mut tx, row, &record).await? {
                        tracing::debug!(
                            canonical_title = %canonical_title,
                            "a second compilation of this title; leaving it as it is"
                        );
                        continue;
                    }
                    // A row made by such files is a compilation from here on.
                    sqlx::query(
                        "UPDATE album
                            SET album_artist = COALESCE(album_artist, ?),
                                is_compilation = 1
                          WHERE id = ?",
                    )
                    .bind(VARIOUS_ARTISTS_LABEL)
                    .bind(row)
                    .execute(&mut *tx)
                    .await?;
                    (row, &record[..])
                }
                None => {
                    let Some((first, rest)) = record.split_first() else {
                        continue;
                    };
                    sqlx::query(
                        "UPDATE album
                            SET artist_id    = ?,
                                album_artist = ?,
                                is_compilation = 1
                          WHERE id = ?",
                    )
                    .bind(va_id)
                    .bind(VARIOUS_ARTISTS_LABEL)
                    .bind(first.id)
                    .execute(&mut *tx)
                    .await?;
                    (first.id, rest)
                }
            };

            for sibling in siblings {
                let sid = sibling.id;
                sqlx::query("UPDATE track SET album_id = ? WHERE album_id = ?")
                    .bind(survivor)
                    .bind(sid)
                    .execute(&mut *tx)
                    .await?;
                // The record lives on under the survivor's id; so does a
                // sidebar pin, which the delete below would cascade away.
                crate::repository::sqlite::sidebar_pin::carry_album_pin(&mut tx, sid, survivor)
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
    }

    tx.commit().await?;
    Ok(())
}

/// Can `record` join the Various Artists row `album_id` already holds? Only
/// beside its tracks, and without a place on the record taken twice.
async fn lies_beside(
    conn: &mut sqlx::SqliteConnection,
    album_id: i64,
    record: &[CandidateAlbum],
) -> CoreResult<bool> {
    let tracks: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT file_path, disc_number, track_number
           FROM track WHERE album_id = ? AND is_available = 1",
    )
    .bind(album_id)
    .fetch_all(&mut *conn)
    .await?;
    let incoming = || record.iter().flat_map(|album| &album.tracks);
    let beside = tracks
        .iter()
        .any(|(path, _, _)| incoming().any(|track| same_directory(path, &track.path)));
    let taken: HashSet<(i64, i64)> = tracks
        .iter()
        .filter_map(|(_, disc, track)| track.map(|track| (disc.unwrap_or(1), track)))
        .collect();
    let collides = incoming().any(|track| track.place.is_some_and(|p| taken.contains(&p)));
    Ok(beside && !collides)
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
