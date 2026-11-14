//! The one-off pass that repairs tags stored before the scanner read legacy
//! code pages (`waveflow_core::scanner::legacy_text`).
//!
//! A track scanned earlier holds its GBK title as `Ò¹Çú`, and the fast path
//! never reopens a file whose mtime and size still match, so without this the
//! repair would only ever reach files that are new or changed. A deep rescan
//! of every folder would do it; the pass does it on the next ordinary scan,
//! for the tracks that can need it and no others.
//!
//! **Which tracks**: those whose stored title, album, artist or album artist
//! is only Latin-1 characters with at least one above ASCII (what a mis-read
//! code page looks like, and "Café" too), and those listed by their file name
//! because lofty once refused their tag. Those files (a few in a Western
//! library, most of a Chinese one) are read again, and
//! one whose text the scanner now reads differently goes through the rewrite
//! branch of the scan as a changed file would: the title, the album and the
//! credits it names are relinked. The others keep their rows untouched.

use std::collections::HashMap;

use sqlx::SqlitePool;
use waveflow_core::scanner::extract::ExtractedFile;

/// `file_path` of each available track of the folder whose text is a
/// candidate, with those texts as stored.
pub(super) async fn suspects(
    pool: &SqlitePool,
    folder_id: i64,
) -> Result<HashMap<String, Vec<String>>, sqlx::Error> {
    // One row per stored text. The artists and the album artist are in
    // `artist`, whichever way the track reaches them.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT t.file_path, t.title
           FROM track t WHERE t.folder_id = ?1 AND t.is_available = 1
         UNION ALL
         SELECT t.file_path, al.title
           FROM track t JOIN album al ON al.id = t.album_id
          WHERE t.folder_id = ?1 AND t.is_available = 1
         UNION ALL
         SELECT t.file_path, ar.name
           FROM track t JOIN track_artist ta ON ta.track_id = t.id
                        JOIN artist ar ON ar.id = ta.artist_id
          WHERE t.folder_id = ?1 AND t.is_available = 1
         UNION ALL
         SELECT t.file_path, ar.name
           FROM track t JOIN album al ON al.id = t.album_id
                        JOIN artist ar ON ar.id = al.artist_id
          WHERE t.folder_id = ?1 AND t.is_available = 1
         UNION ALL
         SELECT t.file_path, ''
           FROM track t
          WHERE t.folder_id = ?1 AND t.is_available = 1 AND t.title_from_filename = 1",
    )
    .bind(folder_id)
    .fetch_all(pool)
    .await?;
    let mut suspects: HashMap<String, Vec<String>> = HashMap::new();
    for (path, text) in rows {
        // The empty text stands for a track listed by its file name.
        if text.is_empty() || waveflow_core::scanner::legacy_text::is_candidate(&text) {
            suspects.entry(path).or_default().push(text);
        }
    }
    Ok(suspects)
}

/// A stored text the file no longer says: it is not in any of the texts the
/// scan just read. An artist stored as one name of a credit is found in the
/// credit (`"A; B"`), so it is a substring test, not an equality.
pub(super) fn stale(stored: &[String], extracted: &ExtractedFile) -> bool {
    let read: [Option<&str>; 4] = [
        Some(extracted.title.as_str()),
        extracted.album.as_deref(),
        extracted.artist.as_deref(),
        extracted.album_artist.as_deref(),
    ];
    stored.iter().any(|text| {
        if text.is_empty() {
            // Listed by its file name because lofty refused its tag: now
            // that the tag reads, the track gets its own title.
            !extracted.title_from_filename
        } else {
            !read.iter().flatten().any(|now| now.contains(text.as_str()))
        }
    })
}
