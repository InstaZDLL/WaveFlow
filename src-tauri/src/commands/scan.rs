use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use chrono::Utc;
use lofty::file::{FileType, TaggedFileExt};
use lofty::picture::MimeType;
use lofty::prelude::{Accessor, AudioFile};
use lofty::tag::{ItemKey, Tag, TagType};
use serde::Serialize;
use sqlx::SqlitePool;
use walkdir::WalkDir;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Extensions considered "audio files" by the scanner. Limited to
/// formats the symphonia + cpal engine can actually decode and play,
/// so the library never displays tracks that would error at play
/// time. Opus / WMA / AIFF are intentionally absent — symphonia
/// doesn't ship a mainline decoder for Opus, WMA is Microsoft
/// proprietary, and AIFF isn't in the default feature set.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "oga", "m4a", "mp4", "aac",
];

/// Outcome of a `scan_folder` call, returned to the frontend so the UI can
/// display a toast like "120 nouveaux titres · 3 mises à jour · 1 erreur".
#[derive(Debug, Serialize, Default)]
pub struct ScanSummary {
    pub folder_id: i64,
    pub scanned: u32,
    pub added: u32,
    pub updated: u32,
    pub skipped: u32,
    pub errors: u32,
    /// Tracks marked `is_available = 0` because their file vanished
    /// from disk between scans. The row stays around (and keeps its
    /// liked / playlist / play-event history) so the user can recover
    /// it by putting the file back.
    pub removed: u32,
}

/// Normalize a title/name for dedup purposes: lowercase + strip punctuation
/// + collapse whitespace. Good enough to match "The Beatles" / "THE  BEATLES"
/// / "the beatles!" onto a single canonical key without pulling in a proper
/// Unicode normalization library.
fn canonical_name(s: &str) -> String {
    s.trim()
        .chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// Stream the file through blake3 in 64 KiB chunks. Full-file hash — slower
/// than a prefix hash but gives us reliable dedup across moved/renamed files.
fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let file = fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Everything the scanner reads off disk for a single audio file. Populated
/// inside `spawn_blocking` so the tokio reactor never stalls on I/O.
struct ExtractedFile {
    abs_path: String,
    size: i64,
    modified_ms: i64,
    hash: String,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    genre: Option<String>,
    year: Option<i64>,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    duration_ms: i64,
    bitrate: Option<i64>,
    sample_rate: Option<i64>,
    channels: Option<i64>,
    /// Bits per sample (16 for CD-quality, 24/32 for Hi-Res masters).
    /// Lossy codecs (MP3, AAC) typically don't expose this — left as
    /// `None` so the UI's Hi-Res badge logic can short-circuit without
    /// inspecting the codec separately.
    bit_depth: Option<i64>,
    /// Short codec / container label inferred from the file type
    /// (e.g. `"FLAC"`, `"MP3"`, `"AAC"`, `"WAV"`). Drives the format
    /// chip on the player footer.
    codec: Option<String>,
    /// Tagged musical key when the file carries one (`TKEY` / ID3v2
    /// or `INITIALKEY` / Vorbis-MP4-APE). Whatever notation the
    /// upstream tagger chose stays as-is — could be `Am`, `F#`, or
    /// the Camelot wheel `8A`.
    musical_key: Option<String>,
    /// Embedded cover art extracted and hash-addressed during the scan. Only
    /// the first picture is kept (lofty exposes them in order and the first
    /// is usually the `CoverFront`). `None` when the tag has no pictures.
    cover_art: Option<ExtractedCover>,
    /// Raw POPM byte (0-255) for ID3v2 files, or a normalised value
    /// derived from the `RATING` text field for Vorbis/FLAC/MP4. `None`
    /// when neither tag carries a rating.
    rating: Option<u8>,
}

struct ExtractedCover {
    /// Hex-encoded blake3 hash of the picture bytes — used as the filename
    /// stem so identical artwork embedded in 20 tracks of an album yields a
    /// single file on disk.
    hash: String,
    /// File extension matching the picture's MIME type (jpg/png/webp/...).
    format: String,
}

/// Map lofty's `FileType` enum to a short uppercase label suitable
/// for the UI's format chip. Falls back to `None` when lofty can't
/// determine a recognized container — we'd rather hide the chip
/// than print "Unknown".
fn file_type_label(ft: FileType) -> Option<String> {
    match ft {
        FileType::Mpeg => Some("MP3".into()),
        FileType::Flac => Some("FLAC".into()),
        FileType::Mp4 => Some("AAC".into()),
        FileType::Aac => Some("AAC".into()),
        FileType::Wav => Some("WAV".into()),
        FileType::Vorbis => Some("Vorbis".into()),
        FileType::Opus => Some("Opus".into()),
        FileType::Aiff => Some("AIFF".into()),
        FileType::Speex => Some("Speex".into()),
        FileType::Ape => Some("APE".into()),
        FileType::WavPack => Some("WavPack".into()),
        FileType::Custom(name) => Some(name.to_string()),
        _ => None,
    }
}

/// Pick a reasonable filename extension for lofty's MIME type enum. Unknown
/// / exotic formats fall through to `"bin"` so the file is still written and
/// the UI can decide what to do with it.
fn extension_for_mime(mime: Option<&MimeType>) -> &'static str {
    match mime {
        Some(MimeType::Jpeg) => "jpg",
        Some(MimeType::Png) => "png",
        Some(MimeType::Gif) => "gif",
        Some(MimeType::Bmp) => "bmp",
        Some(MimeType::Tiff) => "tiff",
        _ => "bin",
    }
}

/// Extract the first picture from the given tag, hash-address it, and write
/// it to `<artwork_dir>/<hash>.<ext>` if missing. Returns the identifying
/// `ExtractedCover` or `None` when the tag has no pictures.
///
/// The write is idempotent: a file whose path already exists is assumed to
/// match (because blake3 hashes are content-addressed), so we never
/// overwrite on re-scan.
fn extract_cover(tag: &Tag, artwork_dir: &Path) -> Option<ExtractedCover> {
    let picture = tag.pictures().first()?;
    let bytes = picture.data();
    if bytes.is_empty() {
        return None;
    }
    let hash = blake3::hash(bytes).to_hex().to_string();
    let format = extension_for_mime(picture.mime_type()).to_string();
    let out_path = artwork_dir.join(format!("{}.{}", &hash, &format));
    if !out_path.exists() {
        if let Err(err) = fs::write(&out_path, bytes) {
            tracing::warn!(path = %out_path.display(), error = %err, "failed to write artwork");
            return None;
        }
    }
    crate::thumbnails::spawn_thumbnail_job(
        out_path,
        artwork_dir.to_path_buf(),
        hash.clone(),
    );
    Some(ExtractedCover { hash, format })
}

/// Extract a 0-255 rating from a tag. POPM frames (ID3v2) are stored by
/// lofty as raw `ItemValue::Binary` under `ItemKey::Popularimeter`: the
/// frame body is `<email>\0<rating:u8><counter:u32+>`, so the rating is
/// the byte right after the first NUL terminator. Vorbis/FLAC/MP4 expose
/// `RATING` as plain text 0-100 which we rescale to 0-255.
fn extract_rating(tag: &Tag) -> Option<u8> {
    if matches!(tag.tag_type(), TagType::Id3v2) {
        if let Some(bytes) = tag.get_binary(&ItemKey::Popularimeter, false) {
            let nul_pos = bytes.iter().position(|b| *b == 0)?;
            return bytes.get(nul_pos + 1).copied();
        }
    }
    if let Some(text) = tag.get_string(&ItemKey::Popularimeter) {
        let trimmed = text.trim();
        if let Ok(val) = trimmed.parse::<u16>() {
            let clamped = val.min(100);
            return Some((clamped * 255 / 100) as u8);
        }
    }
    None
}

/// Read the tagged musical key, if any. ID3v2 stores it as `TKEY`,
/// Vorbis comments / MP4 / APE / WavPack as `INITIALKEY` — lofty
/// unifies both behind `ItemKey::InitialKey`. Empty strings are
/// coalesced to `None` so the UI's "—" placeholder kicks in
/// instead of a blank cell.
fn extract_musical_key(tag: &Tag) -> Option<String> {
    let raw = tag.get_string(&ItemKey::InitialKey)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn extract_file(path: &Path, artwork_dir: &Path) -> Result<ExtractedFile, String> {
    let metadata = fs::metadata(path).map_err(|e| format!("metadata: {e}"))?;
    let size = metadata.len() as i64;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let hash = hash_file(path).map_err(|e| format!("hash: {e}"))?;

    let tagged = lofty::read_from_path(path).map_err(|e| format!("lofty: {e}"))?;
    let props = tagged.properties();
    let duration_ms = props.duration().as_millis() as i64;
    let bitrate = props.audio_bitrate().map(|b| b as i64);
    let sample_rate = props.sample_rate().map(|s| s as i64);
    let channels = props.channels().map(|c| c as i64);
    // Bit depth: lossless codecs report a real PCM bit count; lossy
    // formats either return None or 0 (which we coalesce away so the
    // UI doesn't badge a 320 kbps MP3 as "0-bit Hi-Res").
    let bit_depth = props.bit_depth().map(|b| b as i64).filter(|d| *d > 0);
    let codec = file_type_label(tagged.file_type());

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let (
        title,
        artist,
        album,
        genre,
        year,
        track_number,
        disc_number,
        cover_art,
        rating,
        musical_key,
    ) = match tag {
        Some(tag) => (
            tag.title().map(|s| s.into_owned()),
            tag.artist().map(|s| s.into_owned()),
            tag.album().map(|s| s.into_owned()),
            tag.genre().map(|s| s.into_owned()),
            tag.year().map(|y| y as i64),
            tag.track().map(|n| n as i64),
            tag.disk().map(|n| n as i64),
            extract_cover(tag, artwork_dir),
            extract_rating(tag),
            extract_musical_key(tag),
        ),
        None => (None, None, None, None, None, None, None, None, None, None),
    };

    // Fall back to the file stem when the tag has no title — better than
    // displaying an empty string in the library grid.
    let title = title.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string()
    });

    Ok(ExtractedFile {
        abs_path: path.to_string_lossy().to_string(),
        size,
        modified_ms,
        hash,
        title,
        artist,
        album,
        genre,
        year,
        track_number,
        disc_number,
        duration_ms,
        bitrate,
        sample_rate,
        channels,
        bit_depth,
        codec,
        musical_key,
        cover_art,
        rating,
    })
}

/// Upsert an artwork row keyed on its content hash. Existing rows are
/// returned as-is; new rows are inserted with `source = 'embedded'` so a
/// future cleanup job can distinguish scanner-extracted art from Deezer
/// covers or user-uploaded files.
async fn upsert_artwork(
    pool: &SqlitePool,
    hash: &str,
    format: &str,
) -> AppResult<i64> {
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT id FROM artwork WHERE hash = ?")
            .bind(hash)
            .fetch_optional(pool)
            .await?;
    if let Some(id) = existing {
        return Ok(id);
    }

    let now = now_millis();
    let result = sqlx::query(
        "INSERT INTO artwork (hash, format, source, created_at) VALUES (?, ?, 'embedded', ?)",
    )
    .bind(hash)
    .bind(format)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Split a raw artist string like `"Elior, DJ Garlik"` into individual
/// names. Conservative: only splits on `", "` and `"; "` so that artist
/// names containing `&`, `/`, or `feat.` (e.g. `"AC/DC"`, `"Simon &
/// Garfunkel"`) stay intact.
///
/// Returns the trimmed, non-empty names in the order they appeared —
/// the first entry is treated as the primary artist by the caller.
fn split_artist_name(raw: &str) -> Vec<String> {
    raw.split(|c| c == ',' || c == ';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

async fn upsert_artist(pool: &SqlitePool, raw_name: &str) -> AppResult<Option<i64>> {
    let name = raw_name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let canon = canonical_name(name);
    if canon.is_empty() {
        return Ok(None);
    }

    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM artist WHERE canonical_name = ?",
    )
    .bind(&canon)
    .fetch_optional(pool)
    .await?;
    if let Some(id) = existing {
        return Ok(Some(id));
    }

    let result = sqlx::query("INSERT INTO artist (name, canonical_name) VALUES (?, ?)")
        .bind(name)
        .bind(&canon)
        .execute(pool)
        .await?;
    Ok(Some(result.last_insert_rowid()))
}

async fn upsert_album(
    pool: &SqlitePool,
    title: &str,
    artist_id: Option<i64>,
    year: Option<i64>,
) -> AppResult<Option<i64>> {
    let title = title.trim();
    if title.is_empty() {
        return Ok(None);
    }
    let canon = canonical_name(title);
    if canon.is_empty() {
        return Ok(None);
    }

    // The `UNIQUE (canonical_title, artist_id)` constraint treats NULL as
    // distinct in SQLite, so we dedup manually for the NULL-artist case.
    let existing: Option<i64> = if let Some(aid) = artist_id {
        sqlx::query_scalar(
            "SELECT id FROM album WHERE canonical_title = ? AND artist_id = ?",
        )
        .bind(&canon)
        .bind(aid)
        .fetch_optional(pool)
        .await?
    } else {
        sqlx::query_scalar(
            "SELECT id FROM album WHERE canonical_title = ? AND artist_id IS NULL",
        )
        .bind(&canon)
        .fetch_optional(pool)
        .await?
    };
    if let Some(id) = existing {
        return Ok(Some(id));
    }

    let result = sqlx::query(
        "INSERT INTO album (title, canonical_title, artist_id, year) VALUES (?, ?, ?, ?)",
    )
    .bind(title)
    .bind(&canon)
    .bind(artist_id)
    .bind(year)
    .execute(pool)
    .await?;
    Ok(Some(result.last_insert_rowid()))
}

async fn upsert_genre(pool: &SqlitePool, raw_name: &str) -> AppResult<Option<i64>> {
    let name = raw_name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let canon = canonical_name(name);
    if canon.is_empty() {
        return Ok(None);
    }

    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM genre WHERE canonical_name = ?",
    )
    .bind(&canon)
    .fetch_optional(pool)
    .await?;
    if let Some(id) = existing {
        return Ok(Some(id));
    }

    let result = sqlx::query("INSERT INTO genre (name, canonical_name) VALUES (?, ?)")
        .bind(name)
        .bind(&canon)
        .execute(pool)
        .await?;
    Ok(Some(result.last_insert_rowid()))
}

/// Walk an existing `library_folder` on disk, extract tags from every audio
/// file, and upsert them into the active profile's database.
///
/// New files are inserted, existing rows are updated in place (keying on
/// `(library_id, file_path)`), and files that haven't changed since the last
/// scan — matched on `(file_modified, file_hash)` — are skipped to keep the
/// loop fast on re-scans.
///
/// Failures on individual files are logged but never abort the scan: the
/// summary counter `errors` surfaces them to the UI so the user can tell how
/// many files were rejected.
#[tauri::command]
pub async fn scan_folder(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    folder_id: i64,
) -> AppResult<ScanSummary> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let summary = scan_folder_inner(&pool, &artwork_dir, folder_id).await?;
    // Fire the auto-analyzer in the background when the user has
    // opted in. Spawned so the IPC reply doesn't block on a
    // potentially long analysis pass.
    if summary.added > 0 {
        crate::commands::analysis::maybe_auto_analyze(&app);
    }
    Ok(summary)
}

/// Inner scan implementation shared between the `scan_folder` command and
/// the `rescan_library` command, which walks every folder of a library.
///
/// Takes the resolved database pool + artwork directory directly so it can
/// run in contexts where a `tauri::State` isn't available (e.g. called in a
/// loop from another command).
pub(crate) async fn scan_folder_inner(
    pool: &SqlitePool,
    artwork_dir: &Path,
    folder_id: i64,
) -> AppResult<ScanSummary> {
    // Belt-and-braces: the directory is created at profile bootstrap, but a
    // user fiddling with the data folder could have deleted it.
    std::fs::create_dir_all(artwork_dir)?;

    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT library_id, path FROM library_folder WHERE id = ?",
    )
    .bind(folder_id)
    .fetch_optional(pool)
    .await?;
    let Some((library_id, folder_path)) = row else {
        return Err(AppError::Other(format!("folder {folder_id} not found")));
    };

    // Walk the directory off-thread — walkdir is blocking and a deep tree can
    // take a noticeable fraction of a second to enumerate.
    let folder_path_owned = folder_path.clone();
    let audio_files: Vec<PathBuf> = tokio::task::spawn_blocking(move || {
        WalkDir::new(&folder_path_owned)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
                    .unwrap_or(false)
            })
            .map(|entry| entry.path().to_path_buf())
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("walk task failed: {e}")))?;

    let mut summary = ScanSummary {
        folder_id,
        ..Default::default()
    };
    let now = now_millis();

    // Snapshot of the paths currently flagged available in this folder.
    // We strike each one off as the walk processes it; whatever's left
    // at the end was deleted from disk and gets marked unavailable.
    // Tracks already at `is_available = 0` are excluded — bringing them
    // back is handled by the upsert path which re-sets the flag to 1.
    let mut existing_available: HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT file_path FROM track WHERE folder_id = ? AND is_available = 1",
    )
    .bind(folder_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    for path in audio_files {
        summary.scanned += 1;

        let path_for_task = path.clone();
        let artwork_dir_for_task = artwork_dir.to_path_buf();
        let extracted = match tokio::task::spawn_blocking(move || {
            extract_file(&path_for_task, &artwork_dir_for_task)
        })
        .await
        {
            Ok(Ok(e)) => e,
            Ok(Err(err)) => {
                tracing::warn!(path = %path.display(), error = %err, "extraction failed");
                summary.errors += 1;
                continue;
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "extraction panicked");
                summary.errors += 1;
                continue;
            }
        };

        // File is on disk → keep it out of the deletion sweep below.
        existing_available.remove(&extracted.abs_path);

        let existing: Option<(i64, i64, String)> = sqlx::query_as(
            "SELECT id, file_modified, file_hash FROM track WHERE library_id = ? AND file_path = ?",
        )
        .bind(library_id)
        .bind(&extracted.abs_path)
        .fetch_optional(pool)
        .await?;

        if let Some((existing_track_id, mtime, ref hash)) = existing {
            if mtime == extracted.modified_ms && hash == &extracted.hash {
                // Track content hasn't changed — normally a full skip, but we
                // still backfill cover art when the album is missing one.
                // Scenario: a first scan ran before the scanner extracted
                // embedded pictures, so all existing albums have
                // `artwork_id IS NULL`. A re-scan re-extracts the cover
                // (cheap — the hash-addressed file is idempotent on disk)
                // and we just need to wire it up in the DB.
                if let Some(cover) = &extracted.cover_art {
                    let row: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
                        "SELECT t.album_id, al.artwork_id
                           FROM track t
                           LEFT JOIN album al ON al.id = t.album_id
                          WHERE t.id = ?",
                    )
                    .bind(existing_track_id)
                    .fetch_optional(pool)
                    .await?;
                    if let Some((Some(aid), None)) = row {
                        let artwork_id =
                            upsert_artwork(pool, &cover.hash, &cover.format).await?;
                        sqlx::query("UPDATE album SET artwork_id = ? WHERE id = ?")
                            .bind(artwork_id)
                            .bind(aid)
                            .execute(pool)
                            .await?;
                    }
                }

                // Backfill bit_depth / codec / musical_key for
                // tracks scanned before those migrations shipped.
                // COALESCE keeps any existing value, so a column
                // that's already populated by a more recent scan
                // isn't overwritten with a stale tag re-read.
                sqlx::query(
                    "UPDATE track
                        SET bit_depth   = COALESCE(bit_depth, ?),
                            codec       = COALESCE(codec, ?),
                            musical_key = COALESCE(musical_key, ?)
                      WHERE id = ?",
                )
                .bind(extracted.bit_depth)
                .bind(extracted.codec.as_deref())
                .bind(extracted.musical_key.as_deref())
                .bind(existing_track_id)
                .execute(pool)
                .await?;

                // Reconcile multi-artist splits even when the track content
                // hasn't changed. An earlier scan may have stored
                // "Elior, DJ Garlik" as a single artist; re-running the
                // scanner after we taught it to split should normalize
                // existing rows without requiring a full DB reset.
                if let Some(raw) = &extracted.artist {
                    let splits = split_artist_name(raw);
                    let current_count: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM track_artist WHERE track_id = ?",
                    )
                    .bind(existing_track_id)
                    .fetch_one(pool)
                    .await?;
                    if current_count as usize != splits.len() {
                        let mut ids = Vec::new();
                        for name in splits {
                            if let Some(id) = upsert_artist(pool, &name).await? {
                                ids.push(id);
                            }
                        }
                        sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
                            .bind(existing_track_id)
                            .execute(pool)
                            .await?;
                        for (position, aid) in ids.iter().enumerate() {
                            sqlx::query(
                                "INSERT INTO track_artist (track_id, artist_id, role, position)
                                 VALUES (?, ?, 'main', ?)",
                            )
                            .bind(existing_track_id)
                            .bind(aid)
                            .bind(position as i64)
                            .execute(pool)
                            .await?;
                        }
                        sqlx::query("UPDATE track SET primary_artist = ? WHERE id = ?")
                            .bind(ids.first().copied())
                            .bind(existing_track_id)
                            .execute(pool)
                            .await?;
                        // Also re-link the album to the new primary artist
                        // so "Ma musique > Albums" stays consistent.
                        if let Some(first_id) = ids.first().copied() {
                            sqlx::query(
                                "UPDATE album SET artist_id = ?
                                 WHERE id = (SELECT album_id FROM track WHERE id = ?)
                                   AND artist_id != ?",
                            )
                            .bind(first_id)
                            .bind(existing_track_id)
                            .bind(first_id)
                            .execute(pool)
                            .await?;
                        }
                    }
                }

                summary.skipped += 1;
                continue;
            }
        }

        // Split multi-artist strings (e.g. "Elior, DJ Garlik") so each
        // contributor gets its own row in `artist` and its own link in
        // `track_artist`. The first entry becomes the track's
        // `primary_artist` (and album's `artist_id`) for backwards-
        // compatible ordering.
        let artist_ids: Vec<i64> = match &extracted.artist {
            Some(a) => {
                let mut ids = Vec::new();
                for name in split_artist_name(a) {
                    if let Some(id) = upsert_artist(pool, &name).await? {
                        ids.push(id);
                    }
                }
                ids
            }
            None => Vec::new(),
        };
        let artist_id = artist_ids.first().copied();
        let album_id = match &extracted.album {
            Some(a) => upsert_album(pool, a, artist_id, extracted.year).await?,
            None => None,
        };
        let genre_id = match &extracted.genre {
            Some(g) => upsert_genre(pool, g).await?,
            None => None,
        };

        // Link extracted cover art to the album. Only set it once — we don't
        // want a re-scan to flip the album cover back and forth between
        // variants embedded in different tracks of the same release.
        if let (Some(cover), Some(aid)) = (&extracted.cover_art, album_id) {
            let artwork_id = upsert_artwork(pool, &cover.hash, &cover.format).await?;
            sqlx::query(
                "UPDATE album SET artwork_id = ? WHERE id = ? AND artwork_id IS NULL",
            )
            .bind(artwork_id)
            .bind(aid)
            .execute(pool)
            .await?;
        }

        if let Some((track_id, _, _)) = existing {
            sqlx::query(
                "UPDATE track SET
                    folder_id = ?,
                    file_hash = ?, file_size = ?, file_modified = ?,
                    title = ?, album_id = ?, primary_artist = ?,
                    track_number = ?, disc_number = ?, year = ?,
                    duration_ms = ?, bitrate = ?, sample_rate = ?, channels = ?,
                    bit_depth = ?, codec = ?,
                    musical_key = ?,
                    rating = ?,
                    is_available = 1
                 WHERE id = ?",
            )
            .bind(folder_id)
            .bind(&extracted.hash)
            .bind(extracted.size)
            .bind(extracted.modified_ms)
            .bind(&extracted.title)
            .bind(album_id)
            .bind(artist_id)
            .bind(extracted.track_number)
            .bind(extracted.disc_number)
            .bind(extracted.year)
            .bind(extracted.duration_ms)
            .bind(extracted.bitrate)
            .bind(extracted.sample_rate)
            .bind(extracted.channels)
            .bind(extracted.bit_depth)
            .bind(extracted.codec.as_deref())
            .bind(extracted.musical_key.as_deref())
            .bind(extracted.rating.map(|r| r as i64))
            .bind(track_id)
            .execute(pool)
            .await?;

            sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
                .bind(track_id)
                .execute(pool)
                .await?;
            for (position, aid) in artist_ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO track_artist (track_id, artist_id, role, position)
                     VALUES (?, ?, 'main', ?)",
                )
                .bind(track_id)
                .bind(aid)
                .bind(position as i64)
                .execute(pool)
                .await?;
            }

            sqlx::query("DELETE FROM track_genre WHERE track_id = ?")
                .bind(track_id)
                .execute(pool)
                .await?;
            if let Some(gid) = genre_id {
                sqlx::query("INSERT INTO track_genre (track_id, genre_id) VALUES (?, ?)")
                    .bind(track_id)
                    .bind(gid)
                    .execute(pool)
                    .await?;
            }

            summary.updated += 1;
        } else {
            let insert = sqlx::query(
                "INSERT INTO track (
                    library_id, folder_id, file_path, file_hash, file_size, file_modified,
                    title, album_id, primary_artist,
                    track_number, disc_number, year,
                    duration_ms, bitrate, sample_rate, channels,
                    bit_depth, codec, musical_key,
                    rating,
                    added_at, is_available
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
            )
            .bind(library_id)
            .bind(folder_id)
            .bind(&extracted.abs_path)
            .bind(&extracted.hash)
            .bind(extracted.size)
            .bind(extracted.modified_ms)
            .bind(&extracted.title)
            .bind(album_id)
            .bind(artist_id)
            .bind(extracted.track_number)
            .bind(extracted.disc_number)
            .bind(extracted.year)
            .bind(extracted.duration_ms)
            .bind(extracted.bitrate)
            .bind(extracted.sample_rate)
            .bind(extracted.channels)
            .bind(extracted.bit_depth)
            .bind(extracted.codec.as_deref())
            .bind(extracted.musical_key.as_deref())
            .bind(extracted.rating.map(|r| r as i64))
            .bind(now)
            .execute(pool)
            .await?;
            let track_id = insert.last_insert_rowid();

            for (position, aid) in artist_ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO track_artist (track_id, artist_id, role, position)
                     VALUES (?, ?, 'main', ?)",
                )
                .bind(track_id)
                .bind(aid)
                .bind(position as i64)
                .execute(pool)
                .await?;
            }
            if let Some(gid) = genre_id {
                sqlx::query("INSERT INTO track_genre (track_id, genre_id) VALUES (?, ?)")
                    .bind(track_id)
                    .bind(gid)
                    .execute(pool)
                    .await?;
            }

            summary.added += 1;
        }
    }

    // Anything still in the set was on disk last time but isn't now.
    // Mark it unavailable rather than deleting — preserves play_event
    // history and lets the user "undelete" by restoring the file.
    // SQLite caps bound parameters at ~999, so we update one row at a
    // time. Removed counts are normally tiny (a handful per scan); for
    // bulk wipes the loop is still acceptable since we're already
    // off the audio thread.
    for missing_path in &existing_available {
        let res = sqlx::query(
            "UPDATE track SET is_available = 0
              WHERE folder_id = ? AND file_path = ? AND is_available = 1",
        )
        .bind(folder_id)
        .bind(missing_path)
        .execute(pool)
        .await?;
        if res.rows_affected() > 0 {
            summary.removed += 1;
        }
    }

    sqlx::query("UPDATE library_folder SET last_scanned_at = ? WHERE id = ?")
        .bind(now)
        .bind(folder_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE library SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(library_id)
        .execute(pool)
        .await?;

    tracing::info!(
        folder_id,
        library_id,
        scanned = summary.scanned,
        added = summary.added,
        updated = summary.updated,
        skipped = summary.skipped,
        removed = summary.removed,
        errors = summary.errors,
        "scan complete"
    );

    Ok(summary)
}
