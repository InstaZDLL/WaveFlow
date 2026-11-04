//! The zip plumbing: writing an archive, reading its manifest and
//! extracting it.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{AppError, AppResult};

use super::*;

// ── zip plumbing ────────────────────────────────────────────────────

// `metadata_artwork_dir`: when `Some`, the shared Deezer artwork cache
// is bundled under `metadata_artwork/**` so a restore on another machine
// doesn't have to re-fetch every cover. When `None`, the archive stays
// lean and the cache will be rebuilt lazily after restore.
//
// Written to `<target>.partial` and renamed over `target` only once the
// archive is complete. Writing in place truncated the destination first,
// so an export over an existing archive that failed halfway destroyed
// the copy the user already had, and an automatic backup cut short left
// an archive-shaped file that retention then kept as the newest backup.
pub(crate) fn write_archive(
    target: &Path,
    profile_dir: &Path,
    db_path: &Path,
    artwork_dir: &Path,
    metadata_artwork_dir: Option<&Path>,
    manifest: &ArchiveManifest,
) -> AppResult<()> {
    waveflow_core::atomic_write::write_with(target, |file| {
        write_archive_into(
            file,
            profile_dir,
            db_path,
            artwork_dir,
            metadata_artwork_dir,
            manifest,
        )
    })
}

/// A file removed when it goes out of scope, however the export ends.
struct ScrubbedCopy(std::path::PathBuf);

impl ScrubbedCopy {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScrubbedCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// What stays behind when a profile travels: each table, how to count what it
/// holds of that kind, and how to blank it.
const BLANKED_IN_AN_ARCHIVE: [(&str, &str, &str); 4] = [
    (
        "network_source",
        "SELECT COUNT(*) FROM network_source WHERE secret IS NOT NULL",
        "UPDATE network_source SET secret = NULL",
    ),
    (
        "podcast_channel",
        "SELECT COUNT(*) FROM podcast_channel
          WHERE feed_url IS NOT NULL OR site_url IS NOT NULL OR image_url IS NOT NULL
             OR etag IS NOT NULL OR last_modified IS NOT NULL",
        "UPDATE podcast_channel
            SET feed_url = NULL, site_url = NULL, image_url = NULL,
                etag = NULL, last_modified = NULL",
    ),
    (
        "podcast_episode",
        "SELECT COUNT(*) FROM podcast_episode
          WHERE enclosure_url IS NOT NULL OR chapters_url IS NOT NULL OR transcripts IS NOT NULL",
        "UPDATE podcast_episode
            SET enclosure_url = NULL, chapters_url = NULL, transcripts = NULL",
    ),
    // Not a secret, but not the archive's to carry either: the warning was
    // accepted by a person on a machine.
    (
        "profile_setting",
        "SELECT COUNT(*) FROM profile_setting WHERE key = 'podcasts.enabled'",
        "DELETE FROM profile_setting WHERE key = 'podcasts.enabled'",
    ),
];

/// A copy of the database with every `network_source.secret` and every podcast
/// address blanked. The copy is what gets looked at and what gets archived, even
/// when it holds none: a password saved while an export runs cannot pass between
/// a check of the live file and the read of it.
///
/// A podcast address is here because a private feed holds its token in it, in
/// the path as often as in the query, and the enclosures after it often do too.
/// What survives is the hash a channel and an episode are known by, so
/// subscribing to the same address again finds them with their positions. The
/// deliberate way to carry addresses is an OPML export.
///
/// `VACUUM` is the point as much as the `UPDATE`, and it runs whatever was blanked:
/// an updated row leaves its old bytes in the page, a removed one in a free page,
/// and the archive is the whole file. Any write that makes a podcast row grow
/// moves it, a position or an error word included, so no list of careful writers
/// could keep an address out of those bytes, and a profile with no channel left has
/// no row to blank but may still hold every address it ever had. The rebuild leaves
/// nothing but what the rows now say.
///
/// `write_archive` is a plain blocking function, so this drives sqlx on a thread
/// of its own, which works wherever it was called from.
fn database_without_secrets(db_path: &Path) -> AppResult<ScrubbedCopy> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| scrub_on_this_thread(db_path))
            .join()
            .map_err(|_| AppError::Other("export: the database copy panicked".into()))?
    })
}

fn scrub_on_this_thread(db_path: &Path) -> AppResult<ScrubbedCopy> {
    use sqlx::{sqlite::SqliteConnectOptions, ConnectOptions, Connection};

    let copy = ScrubbedCopy(
        db_path.with_extension(format!("export-{}.tmp", uuid::Uuid::new_v4().simple())),
    );
    std::fs::copy(db_path, copy.path())?;
    crate::netlib::block_on(async {
        let mut conn = SqliteConnectOptions::new()
            .filename(copy.path())
            .connect()
            .await?;
        sqlx::query("PRAGMA journal_mode = DELETE")
            .execute(&mut conn)
            .await?;
        blank_what_does_not_travel(&mut conn).await?;
        sqlx::query("VACUUM").execute(&mut conn).await?;
        conn.close().await
    })?;
    Ok(copy)
}

/// Runs [`BLANKED_IN_AN_ARCHIVE`] over a database. On a copy being archived, and
/// again on a database just imported, since an archive may have been made by
/// another build or by hand: a podcast address in one is not an address this
/// person typed, and the refresh timer would otherwise contact what it names, the
/// local network included, as soon as the warning is accepted here.
pub(super) async fn blank_what_does_not_travel(
    conn: &mut sqlx::SqliteConnection,
) -> Result<(), sqlx::Error> {
    for (table, count, blank) in BLANKED_IN_AN_ARCHIVE {
        // A profile not migrated since has no such table, and that alone means
        // nothing to blank.
        let has_table: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(&mut *conn)
        .await?;
        if has_table == 0 {
            continue;
        }
        let found: i64 = sqlx::query_scalar(count).fetch_one(&mut *conn).await?;
        if found > 0 {
            sqlx::query(blank).execute(&mut *conn).await?;
        }
    }
    Ok(())
}

fn write_archive_into(
    file: &mut File,
    profile_dir: &Path,
    db_path: &Path,
    artwork_dir: &Path,
    metadata_artwork_dir: Option<&Path>,
    manifest: &ArchiveManifest,
) -> AppResult<()> {
    let mut zip = ZipWriter::new(file);

    let opts: SimpleFileOptions = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    // 1. Manifest.
    let manifest_json = serde_json::to_vec_pretty(manifest)
        .map_err(|e| AppError::Other(format!("manifest serialize: {e}")))?;
    zip.start_file(MANIFEST_FILENAME, opts)?;
    zip.write_all(&manifest_json)?;

    // 2. data.db — copy the full file (already checkpointed by the
    //    caller when it belongs to the active profile).
    if db_path.exists() {
        zip.start_file("data.db", opts)?;
        // A share's password is the password of an account on someone's NAS,
        // and an archive is made to travel. A copy with it blanked goes in
        // instead; an imported profile asks for it again.
        let scrubbed = database_without_secrets(db_path)?;
        let mut src = File::open(scrubbed.path())?;
        std::io::copy(&mut src, &mut zip)?;
    }

    // 3. artwork/** — recursive walk so we keep the same shape inside
    //    the archive as on disk. Empty directories are skipped (zip
    //    libraries treat them as no-ops anyway).
    if artwork_dir.exists() {
        for entry in WalkDir::new(artwork_dir).into_iter().flatten() {
            let entry_path = entry.path();
            if !entry_path.is_file() {
                continue;
            }
            let rel = entry_path
                .strip_prefix(profile_dir)
                .map_err(|e| AppError::Other(format!("artwork rel: {e}")))?;
            let zip_name = rel.to_string_lossy().replace('\\', "/");
            zip.start_file(&zip_name, opts)?;
            let mut src = File::open(entry_path)?;
            std::io::copy(&mut src, &mut zip)?;
        }
    }

    // 4. metadata_artwork/** — shared Deezer cache, opt-out. Files are
    //    already JPEG/PNG/WebP so deflate barely helps, but we keep the
    //    same compression setting for archive uniformity.
    if let Some(meta_dir) = metadata_artwork_dir {
        if meta_dir.exists() {
            for entry in WalkDir::new(meta_dir).into_iter().flatten() {
                let entry_path = entry.path();
                if !entry_path.is_file() {
                    continue;
                }
                let rel = entry_path
                    .strip_prefix(meta_dir)
                    .map_err(|e| AppError::Other(format!("metadata_artwork rel: {e}")))?;
                let zip_name = format!(
                    "metadata_artwork/{}",
                    rel.to_string_lossy().replace('\\', "/")
                );
                zip.start_file(&zip_name, opts)?;
                let mut src = File::open(entry_path)?;
                std::io::copy(&mut src, &mut zip)?;
            }
        }
    }

    zip.finish()?;
    Ok(())
}

pub(super) fn read_manifest(source: &Path) -> AppResult<ArchiveManifest> {
    let file = File::open(source)?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| AppError::Other(format!("open archive: {e}")))?;
    let mut manifest_file = archive
        .by_name(MANIFEST_FILENAME)
        .map_err(|e| AppError::Other(format!("missing manifest.json: {e}")))?;
    let mut buf = String::new();
    manifest_file.read_to_string(&mut buf)?;
    let manifest: ArchiveManifest =
        serde_json::from_str(&buf).map_err(|e| AppError::Other(format!("decode manifest: {e}")))?;
    Ok(manifest)
}

// `metadata_artwork_dir`: destination for `metadata_artwork/**` entries
// — the shared cache directory, NOT the per-profile one. Existing files
// in the cache are preserved (overwritten only if the archive carries
// them).
pub(super) fn extract_archive(
    source: &Path,
    profile_dir: &Path,
    db_path: &Path,
    artwork_dir: &Path,
    metadata_artwork_dir: &Path,
) -> AppResult<()> {
    let file = File::open(source)?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| AppError::Other(format!("open archive: {e}")))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::Other(format!("read archive entry {i}: {e}")))?;
        if entry.is_dir() {
            continue;
        }
        // `entry.enclosed_name()` rejects absolute / parent-traversal
        // paths — basic zip-slip protection.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let rel: PathBuf = rel;
        let name = rel.to_string_lossy();

        if name == MANIFEST_FILENAME {
            continue; // manifest stays in the archive only
        }
        if name == "data.db" {
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = File::create(db_path)?;
            std::io::copy(&mut entry, &mut out)?;
            continue;
        }
        if name.starts_with("artwork/") || name.starts_with("artwork\\") {
            let rel_in_artwork = rel
                .strip_prefix("artwork")
                .map_err(|e| AppError::Other(format!("artwork strip: {e}")))?;
            let dest = artwork_dir.join(rel_in_artwork);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = File::create(&dest)?;
            std::io::copy(&mut entry, &mut out)?;
            continue;
        }
        if name.starts_with("metadata_artwork/") || name.starts_with("metadata_artwork\\") {
            let rel_in_meta = rel
                .strip_prefix("metadata_artwork")
                .map_err(|e| AppError::Other(format!("metadata_artwork strip: {e}")))?;
            let dest = metadata_artwork_dir.join(rel_in_meta);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = File::create(&dest)?;
            std::io::copy(&mut entry, &mut out)?;
            continue;
        }
        // Unknown entries are ignored — keeps forward compat if a
        // future export adds files we don't yet understand.
        let _ = (profile_dir, &name);
    }
    Ok(())
}
