//! Sidecar artist images: link a local picture to its `artist` row,
//! the per-scan walk over every artist's folders, and the Various
//! Artists sidecar.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::CoreResult;
use crate::scanner::extract::{
    extract_artist_image, extract_artist_image_cached, ArtistImageDirCache, ExtractedCover,
};

use super::{canonical_name, split_artist_name, upsert_artwork, VARIOUS_ARTISTS_LABEL};

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
