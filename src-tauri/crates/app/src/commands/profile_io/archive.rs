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
        let mut src = File::open(db_path)?;
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
