//! Profile export / import.
//!
//! Bundles a profile's per-user state into a single `.waveflow` (zip)
//! file the user can shuttle between machines or stash as a backup.
//!
//! What ends up in the archive:
//!   - `manifest.json` — schema version, profile name, export timestamp.
//!     A future version bump lets us refuse incompatible imports rather
//!     than silently breaking on a missing column.
//!   - `data.db` — the per-profile SQLite database (playlists, liked,
//!     stats, EQ, sleep-timer / A-B visibility, shortcut overrides…).
//!   - `artwork/**` — manual covers the user uploaded.
//!   - `metadata_artwork/**` — *optional*, gated by the
//!     `backup.include_metadata_artwork` app setting (default ON). This
//!     is the shared Deezer cover/artist-picture cache; bundling it
//!     means a restore on an offline machine still shows the artwork
//!     immediately. Users who want lean archives can disable the toggle
//!     in Settings → Sauvegardes and re-fetch on demand via "Récupérer
//!     toutes les pochettes manquantes".
//!
//! What we deliberately **don't** bundle:
//!   - The shared `app.db` (Last.fm key, Discord opt-in, app-wide
//!     settings) — those belong to the install, not the profile.
//!   - WAL / SHM sidecars — we run a `WAL_CHECKPOINT(TRUNCATE)` before
//!     copying so the bundled `data.db` is self-contained.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, SqlitePool};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::{
    db,
    error::{AppError, AppResult},
    paths::AppPaths,
    state::AppState,
};

/// Bumped when the on-disk shape of a `.waveflow` archive changes in
/// an incompatible way (renamed manifest fields, removed `artwork/`
/// dir, etc.). Schema-level differences inside `data.db` are caught
/// by sqlx migration replay at first switch — see `import_profile`.
pub(crate) const ARCHIVE_VERSION: u32 = 1;

const MANIFEST_FILENAME: &str = "manifest.json";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ArchiveManifest {
    pub archive_version: u32,
    /// `CARGO_PKG_VERSION` of the WaveFlow build that produced the
    /// archive. Surfaced for diagnostics; not used for compatibility
    /// gating (versions diverge faster than the archive shape).
    pub app_version: String,
    pub profile_name: String,
    /// Source profile id at export time. Purely informational — the
    /// new profile gets a fresh id at import.
    pub source_profile_id: i64,
    pub exported_at: String,
}

/// Export the active profile (or `profile_id` if provided) into a
/// `.waveflow` archive at `target_path`. Overwrites if the file
/// already exists.
#[tauri::command]
pub async fn export_profile(
    state: tauri::State<'_, AppState>,
    profile_id: Option<i64>,
    target_path: String,
) -> AppResult<()> {
    let profile_id = match profile_id {
        Some(id) => id,
        None => state.require_profile_id().await?,
    };

    // Look up the source profile name for the manifest. Fail loudly if
    // the row is gone — caller passed a stale id.
    let profile_name: String = sqlx::query_scalar("SELECT name FROM profile WHERE id = ?")
        .bind(profile_id)
        .fetch_optional(&state.app_db)
        .await?
        .ok_or(AppError::ProfileNotFound(profile_id))?;

    // If we're exporting the currently active profile, make sure any
    // pending WAL pages are folded back into the main file before we
    // copy it — otherwise the archive holds a partial snapshot.
    let active_id = {
        let guard = state.profile.read().await;
        guard.as_ref().map(|p| p.profile_id)
    };
    if active_id == Some(profile_id) {
        if let Ok(pool) = state.require_profile_pool().await {
            checkpoint_wal(&pool).await?;
        }
    }

    let profile_dir = state.paths.profile_dir(profile_id);
    let db_path = state.paths.profile_db(profile_id);
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_artwork_dir = if read_include_metadata_artwork(&state.app_db).await? {
        Some(state.paths.metadata_artwork_dir.clone())
    } else {
        None
    };

    let manifest = ArchiveManifest {
        archive_version: ARCHIVE_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        profile_name,
        source_profile_id: profile_id,
        exported_at: Utc::now().to_rfc3339(),
    };

    // CPU-bound work (zip + read all artwork) → spawn_blocking so the
    // tokio runtime stays responsive while a multi-GB library is
    // packaged. The closure owns its inputs by value to side-step the
    // 'static bound spawn_blocking imposes.
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        write_archive(
            Path::new(&target_path),
            &profile_dir,
            &db_path,
            &artwork_dir,
            metadata_artwork_dir.as_deref(),
            &manifest,
        )
    })
    .await
    .map_err(|e| AppError::Other(format!("export task join: {e}")))??;

    Ok(())
}

/// Read the `backup.include_metadata_artwork` app setting. Defaults to
/// `true` so a fresh install + first manual export produces a complete
/// archive without the user having to opt in.
pub(crate) async fn read_include_metadata_artwork(app_db: &SqlitePool) -> AppResult<bool> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = 'backup.include_metadata_artwork'",
    )
    .fetch_optional(app_db)
    .await?;
    Ok(row.map(|v| v == "true" || v == "1").unwrap_or(true))
}

/// Import a `.waveflow` archive as a brand-new profile. The new
/// profile is **not** activated automatically — the caller can switch
/// to it via `switch_profile` once the user picks it from the
/// selector. Returns the new profile id.
#[tauri::command]
pub async fn import_profile(
    state: tauri::State<'_, AppState>,
    source_path: String,
    name: Option<String>,
) -> AppResult<i64> {
    // 1. Inspect the archive on a blocking thread (file I/O + zip
    //    decompression). Manifest parsing happens here so we fail fast
    //    on a truly broken file before touching the DB.
    let manifest = tokio::task::spawn_blocking({
        let source_path = source_path.clone();
        move || read_manifest(Path::new(&source_path))
    })
    .await
    .map_err(|e| AppError::Other(format!("import inspect join: {e}")))??;

    if manifest.archive_version != ARCHIVE_VERSION {
        return Err(AppError::Other(format!(
            "incompatible archive (version {}, expected {})",
            manifest.archive_version, ARCHIVE_VERSION
        )));
    }

    // 2. Allocate a fresh profile row. We reuse the standard creation
    //    path so the row + filesystem layout match what `create_profile`
    //    would produce — minus the empty data.db, which we overwrite
    //    from the archive in step 3.
    let profile_name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or(manifest.profile_name.clone());

    let now = Utc::now().timestamp_millis();
    let insert = sqlx::query(
        "INSERT INTO profile (name, color_id, avatar_hash, data_dir, created_at, last_used_at)
         VALUES (?, 'emerald', NULL, '', ?, ?)",
    )
    .bind(&profile_name)
    .bind(now)
    .bind(now)
    .execute(&state.app_db)
    .await?;

    let new_profile_id = insert.last_insert_rowid();
    let rel_dir = AppPaths::profile_rel_dir(new_profile_id);
    sqlx::query("UPDATE profile SET data_dir = ? WHERE id = ?")
        .bind(&rel_dir)
        .bind(new_profile_id)
        .execute(&state.app_db)
        .await?;

    state.paths.ensure_profile_dirs(new_profile_id)?;

    let new_profile_dir = state.paths.profile_dir(new_profile_id);
    let new_db_path = state.paths.profile_db(new_profile_id);
    let new_artwork_dir = state.paths.profile_artwork_dir(new_profile_id);
    let metadata_artwork_dir = state.paths.metadata_artwork_dir.clone();

    // 3. Extract — also blocking. On any failure, roll the profile row
    //    back so the user doesn't end up with a stub profile that
    //    points at a half-written directory.
    let extract_result = tokio::task::spawn_blocking(move || {
        extract_archive(
            Path::new(&source_path),
            &new_profile_dir,
            &new_db_path,
            &new_artwork_dir,
            &metadata_artwork_dir,
        )
    })
    .await
    .map_err(|e| AppError::Other(format!("import extract join: {e}")))?;

    if let Err(err) = extract_result {
        cleanup_partial_profile(&state, new_profile_id).await;
        return Err(err);
    }

    // 4. Normalise the bundled `_sqlx_migrations.checksum` column against
    //    the local migration files before running the migrator. Archives
    //    produced by a build whose migration files happened to be
    //    checked out with CRLF endings (Git `core.autocrlf=true` on
    //    Windows + no `.gitattributes` lock) store SHA-384 hashes
    //    computed on different bytes than the current LF-normalised
    //    sources, even though the SQL is semantically identical. Without
    //    this step sqlx refuses the import with
    //    "migration X was previously applied but has been modified".
    //    See `.gitattributes` for the forward fix.
    if let Err(err) = normalise_migration_checksums(&state.paths.profile_db(new_profile_id)).await {
        cleanup_partial_profile(&state, new_profile_id).await;
        return Err(err);
    }

    // 5. Open + close the imported pool once so any pending migrations
    //    (the source might be older than the local schema) replay
    //    immediately. This matches the create_profile flow and gives
    //    the user a usable profile by the time the call returns.
    let pool =
        match db::profile_db::open(&state.paths.profile_db(new_profile_id), &state.paths.app_db)
            .await
        {
            Ok(pool) => pool,
            Err(err) => {
                cleanup_partial_profile(&state, new_profile_id).await;
                return Err(err);
            }
        };
    pool.close().await;

    Ok(new_profile_id)
}

/// Roll back a half-imported profile: remove the on-disk directory and
/// the `profile` row. Best-effort — failures are swallowed so the caller
/// can surface the *original* import error rather than a cleanup error
/// masking it. User can wipe `<app_data>/profiles/<id>/` by hand if the
/// fs delete failed (rare; usually a held file handle from a panicked
/// pool).
async fn cleanup_partial_profile(state: &AppState, profile_id: i64) {
    let _ = std::fs::remove_dir_all(state.paths.profile_dir(profile_id));
    let _ = sqlx::query("DELETE FROM profile WHERE id = ?")
        .bind(profile_id)
        .execute(&state.app_db)
        .await;
}

// ── zip plumbing ────────────────────────────────────────────────────

// `metadata_artwork_dir`: when `Some`, the shared Deezer artwork cache
// is bundled under `metadata_artwork/**` so a restore on another machine
// doesn't have to re-fetch every cover. When `None`, the archive stays
// lean and the cache will be rebuilt lazily after restore.
pub(crate) fn write_archive(
    target: &Path,
    profile_dir: &Path,
    db_path: &Path,
    artwork_dir: &Path,
    metadata_artwork_dir: Option<&Path>,
    manifest: &ArchiveManifest,
) -> AppResult<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(target)?;
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

fn read_manifest(source: &Path) -> AppResult<ArchiveManifest> {
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
fn extract_archive(
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

// ── helpers ────────────────────────────────────────────────────────

/// Rewrite `_sqlx_migrations.checksum` for every previously-applied
/// migration so it matches the SHA-384 of the *local* migration file
/// bundled into the running binary. Called on a freshly extracted
/// `data.db` before the sqlx migrator runs.
///
/// Two failure modes the caller surfaces verbatim:
///   - Local migrator missing a version present in the archive
///     → the archive is from a *newer* build than the one importing it,
///     and we genuinely can't roll the schema forward.
///   - Anything else → propagated as a generic `Other` error.
///
/// Same-version + same-content but different-checksum is treated as
/// benign byte-level drift (line endings, BOM) and silently fixed: the
/// "migrations are immutable once merged" rule means a version that
/// exists in both sides represents the same DDL by construction.
async fn normalise_migration_checksums(db_path: &Path) -> AppResult<()> {
    let migrator = sqlx::migrate!("./migrations/profile");

    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false)
        // Skip the noisy "executing statement" log line on every checksum
        // UPDATE — these are pure plumbing rewrites, not user-visible
        // DB activity.
        .disable_statement_logging();
    let mut conn = opts.connect().await?;

    // The archive may predate the introduction of `_sqlx_migrations`
    // (very unlikely, but we don't want to crash on the bootstrap case).
    let table_exists: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_optional(&mut conn)
    .await?;
    if table_exists.is_none() {
        conn.close().await?;
        return Ok(());
    }

    let stored: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations")
            .fetch_all(&mut conn)
            .await?;

    for (version, stored_checksum) in stored {
        let local = migrator.iter().find(|m| m.version == version);
        let Some(local) = local else {
            return Err(AppError::Other(format!(
                "archive contains migration {version} not present in this build — \
                 export was produced by a newer WaveFlow version"
            )));
        };
        if local.checksum.as_ref() == stored_checksum.as_slice() {
            continue;
        }
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(local.checksum.as_ref())
            .bind(version)
            .execute(&mut conn)
            .await?;
    }

    conn.close().await?;
    Ok(())
}

/// Force a full WAL checkpoint so the archive captures every committed
/// page. `TRUNCATE` resets the WAL file to zero length on success,
/// which also keeps `.waveflow` archives from carrying a stale
/// sidecar's worth of bytes.
pub(crate) async fn checkpoint_wal(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await?;
    Ok(())
}
