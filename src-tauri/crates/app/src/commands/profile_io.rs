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

/// What a successful import hands back: the new profile's id and the
/// name it was filed under, which is the archive's own unless another
/// profile already held it (#767).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedProfile {
    pub id: i64,
    pub name: String,
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
    // The UI can only say "export failed"; the reason goes to the log,
    // where the message points the user.
    export_profile_inner(&state, profile_id, target_path)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "profile export failed"))
}

async fn export_profile_inner(
    state: &AppState,
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
/// selector. Returns the new profile's id and name.
#[tauri::command]
pub async fn import_profile(
    state: tauri::State<'_, AppState>,
    source_path: String,
    name: Option<String>,
) -> AppResult<ImportedProfile> {
    // Same as the export: the UI has one generic sentence, the log has
    // the cause. #767 took a user report to find a UNIQUE violation.
    import_profile_inner(&state, source_path, name)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "profile import failed"))
}

async fn import_profile_inner(
    state: &AppState,
    source_path: String,
    name: Option<String>,
) -> AppResult<ImportedProfile> {
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

    let (new_profile_id, profile_name) =
        insert_with_free_name(&state.app_db, &profile_name).await?;
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
        cleanup_partial_profile(state, new_profile_id).await;
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
        cleanup_partial_profile(state, new_profile_id).await;
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
                cleanup_partial_profile(state, new_profile_id).await;
                return Err(err);
            }
        };
    pool.close().await;

    Ok(ImportedProfile {
        id: new_profile_id,
        name: profile_name,
    })
}

/// The most "(n)" suffixes an import tries before giving up. A person
/// with a thousand profiles of one name has a problem this won't solve.
const MAX_NAME_SUFFIX: u32 = 1000;

/// Insert the imported profile's row under `base`, or under `base (2)`,
/// `base (3)`… when that name is taken (#767).
///
/// `profile.name` is UNIQUE, and restoring a backup of a profile that
/// still exists is the ordinary case, not the odd one: the archive's name
/// is taken by construction. The user never chose that name, so the app
/// finds a free one rather than refusing. Retrying on the violation
/// itself, instead of checking first, leaves no window for a profile
/// created meanwhile to take the name between the check and the insert.
async fn insert_with_free_name(app_db: &SqlitePool, base: &str) -> AppResult<(i64, String)> {
    let now = Utc::now().timestamp_millis();
    for n in 1..=MAX_NAME_SUFFIX {
        let candidate = if n == 1 {
            base.to_string()
        } else {
            format!("{base} ({n})")
        };
        let inserted = sqlx::query(
            "INSERT INTO profile (name, color_id, avatar_hash, data_dir, created_at, last_used_at)
             VALUES (?, 'emerald', NULL, '', ?, ?)",
        )
        .bind(&candidate)
        .bind(now)
        .bind(now)
        .execute(app_db)
        .await;
        match inserted {
            Ok(done) => return Ok((done.last_insert_rowid(), candidate)),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(AppError::Other(format!(
        "no free profile name left for \"{base}\" after {MAX_NAME_SUFFIX} tries"
    )))
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

// ── preview ────────────────────────────────────────────────────────

/// Prefix of the error a preview returns when the file is not a readable
/// WaveFlow archive — not a zip, no manifest, no database, a database that
/// fails its integrity check. Stable so the dialog can translate it instead
/// of showing the technical message that follows.
pub(crate) const ERR_UNREADABLE: &str = "archive: unreadable";

/// What a `.waveflow` archive holds, read before anything is imported.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivePreview {
    pub profile_name: String,
    /// The WaveFlow version that wrote the archive.
    pub app_version: String,
    pub exported_at: String,
    pub archive_version: u32,
    /// The newest migration recorded in the archive's database, `None` when
    /// it records none (or was not read, see `contents`).
    pub schema_version: Option<i64>,
    /// The newest migration this build ships, to set `schema_version`
    /// against.
    pub local_schema_version: i64,
    pub compatibility: ArchiveCompatibility,
    /// `None` when the archive's layout is not one this build reads
    /// (`compatibility` says which): its database is then left unopened.
    pub contents: Option<ArchiveContents>,
    pub artwork_files: u64,
    pub metadata_artwork_files: u64,
    pub size_bytes: u64,
}

/// Whether [`import_profile`] would take the archive, and if not, why.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ArchiveCompatibility {
    Compatible,
    /// Written in an archive layout newer than this build knows.
    NewerArchive,
    /// Written in an archive layout this build no longer reads.
    UnsupportedArchive,
    /// Its database went through a migration this build does not have.
    NewerSchema {
        version: i64,
    },
    /// Its build applied other SQL under a migration this build has.
    DifferentBuild {
        version: i64,
    },
}

/// Row counts of the archive's database. A table the archive's schema does
/// not have yet counts as empty.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveContents {
    pub tracks: i64,
    pub playlists: i64,
    pub smart_playlists: i64,
    pub liked_tracks: i64,
    pub plays: i64,
    pub library_folders: i64,
    pub settings: i64,
}

/// Read a `.waveflow` archive without importing it: its manifest, the
/// number of things its database holds, and whether this build would
/// import it.
///
/// Read-only. The database is copied to a scratch file in the system's
/// temporary directory — SQLite cannot read it inside the zip — opened
/// read-only, and deleted on the way out; no profile is created and
/// nothing in the app's data directory is touched.
#[tauri::command]
pub async fn preview_profile_archive(source_path: String) -> AppResult<ArchivePreview> {
    preview_archive(Path::new(&source_path))
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "profile archive preview failed"))
}

async fn preview_archive(source: &Path) -> AppResult<ArchivePreview> {
    let scratch = ScratchFile::new("db");
    let inventory = tokio::task::spawn_blocking({
        let source = source.to_path_buf();
        let db = scratch.path.clone();
        move || inspect_archive(&source, &db)
    })
    .await
    .map_err(|e| AppError::Other(format!("preview inspect join: {e}")))??;
    let manifest = inventory.manifest;

    let migrator = sqlx::migrate!("../../migrations/profile");
    let local_schema_version = migrator.iter().map(|m| m.version).max().unwrap_or(0);

    let layout = match manifest.archive_version {
        ARCHIVE_VERSION => None,
        newer if newer > ARCHIVE_VERSION => Some(ArchiveCompatibility::NewerArchive),
        _ => Some(ArchiveCompatibility::UnsupportedArchive),
    };
    let (compatibility, contents, schema_version) = match layout {
        Some(refusal) => (refusal, None, None),
        None => {
            let read = read_archive_db(&scratch.path).await?;
            let compatibility = match judge_migrations(&migrator, &read.migrations) {
                MigrationVerdict::Compatible { .. } => ArchiveCompatibility::Compatible,
                MigrationVerdict::NewerBuild { version } => {
                    ArchiveCompatibility::NewerSchema { version }
                }
                MigrationVerdict::DifferentSql { version, .. } => {
                    ArchiveCompatibility::DifferentBuild { version }
                }
            };
            let schema_version = read.migrations.iter().map(|(v, _)| *v).max();
            (compatibility, Some(read.contents), schema_version)
        }
    };

    Ok(ArchivePreview {
        profile_name: manifest.profile_name,
        app_version: manifest.app_version,
        exported_at: manifest.exported_at,
        archive_version: manifest.archive_version,
        schema_version,
        local_schema_version,
        compatibility,
        contents,
        artwork_files: inventory.artwork_files,
        metadata_artwork_files: inventory.metadata_artwork_files,
        size_bytes: inventory.size_bytes,
    })
}

fn unreadable(detail: impl std::fmt::Display) -> AppError {
    AppError::Other(format!("{ERR_UNREADABLE}: {detail}"))
}

/// A file in the system's temporary directory that is removed when this
/// goes out of scope, whichever way the preview ends.
struct ScratchFile {
    path: PathBuf,
}

impl ScratchFile {
    fn new(extension: &str) -> Self {
        let name = format!("waveflow-preview-{}.{extension}", uuid::Uuid::new_v4());
        Self {
            path: std::env::temp_dir().join(name),
        }
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What the zip itself says, before its database is opened.
struct ArchiveInventory {
    manifest: ArchiveManifest,
    artwork_files: u64,
    metadata_artwork_files: u64,
    size_bytes: u64,
}

/// Read the manifest, count the artwork entries and copy `data.db` to
/// `db_out`. Any of those failing is the archive being unreadable.
fn inspect_archive(source: &Path, db_out: &Path) -> AppResult<ArchiveInventory> {
    let file = File::open(source)?;
    let size_bytes = file.metadata()?.len();
    let mut archive = ZipArchive::new(file).map_err(|e| unreadable(format!("not a zip: {e}")))?;

    let manifest: ArchiveManifest = {
        let mut entry = archive
            .by_name(MANIFEST_FILENAME)
            .map_err(|e| unreadable(format!("no manifest.json: {e}")))?;
        let mut buf = String::new();
        entry
            .read_to_string(&mut buf)
            .map_err(|e| unreadable(format!("read manifest: {e}")))?;
        serde_json::from_str(&buf).map_err(|e| unreadable(format!("decode manifest: {e}")))?
    };

    let mut artwork_files = 0;
    let mut metadata_artwork_files = 0;
    let mut has_db = false;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| unreadable(format!("entry {i}: {e}")))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .name()
            .map_err(|e| unreadable(format!("entry {i}: {e}")))?
            .replace('\\', "/");
        if name.starts_with("artwork/") {
            artwork_files += 1;
        } else if name.starts_with("metadata_artwork/") {
            metadata_artwork_files += 1;
        } else if name == "data.db" {
            let mut out = File::create(db_out)?;
            std::io::copy(&mut entry, &mut out).map_err(|e| unreadable(format!("data.db: {e}")))?;
            has_db = true;
        }
    }
    if !has_db {
        return Err(unreadable("no data.db"));
    }
    Ok(ArchiveInventory {
        manifest,
        artwork_files,
        metadata_artwork_files,
        size_bytes,
    })
}

/// The archive's database, as far as a preview needs it.
struct ArchiveDb {
    contents: ArchiveContents,
    migrations: Vec<(i64, Vec<u8>)>,
}

/// Open the copied database read-only, check it, and count what it holds.
async fn read_archive_db(path: &Path) -> AppResult<ArchiveDb> {
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true)
        .disable_statement_logging()
        .connect()
        .await
        .map_err(|e| unreadable(format!("open data.db: {e}")))?;
    let read = read_archive_db_on(&mut conn).await;
    // Closed before the scratch file is removed: Windows keeps an open file.
    let _ = conn.close().await;
    read
}

async fn read_archive_db_on(conn: &mut sqlx::SqliteConnection) -> AppResult<ArchiveDb> {
    // A file that is not a database, or one cut short, fails here rather
    // than producing counts from whatever pages survived.
    let check: String = sqlx::query_scalar("PRAGMA quick_check(1)")
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| unreadable(format!("data.db: {e}")))?;
    if check != "ok" {
        return Err(unreadable(format!(
            "data.db fails its integrity check: {check}"
        )));
    }

    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| unreadable(format!("data.db: {e}")))?;
    let has = |table: &str| tables.iter().any(|t| t == table);

    let mut contents = ArchiveContents::default();
    for (table, query, slot) in [
        ("track", "SELECT count(*) FROM track", &mut contents.tracks),
        (
            "playlist",
            "SELECT count(*) FROM playlist WHERE is_smart = 0",
            &mut contents.playlists,
        ),
        (
            "playlist",
            "SELECT count(*) FROM playlist WHERE is_smart <> 0",
            &mut contents.smart_playlists,
        ),
        (
            "liked_track",
            "SELECT count(*) FROM liked_track",
            &mut contents.liked_tracks,
        ),
        (
            "play_event",
            "SELECT count(*) FROM play_event",
            &mut contents.plays,
        ),
        (
            "library_folder",
            "SELECT count(*) FROM library_folder",
            &mut contents.library_folders,
        ),
        (
            "profile_setting",
            "SELECT count(*) FROM profile_setting",
            &mut contents.settings,
        ),
    ] {
        if !has(table) {
            continue;
        }
        *slot = sqlx::query_scalar(query)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| unreadable(format!("count {table}: {e}")))?;
    }

    let migrations = if has("_sqlx_migrations") {
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations")
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| unreadable(format!("data.db migrations: {e}")))?
    } else {
        Vec::new()
    };
    Ok(ArchiveDb {
        contents,
        migrations,
    })
}

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
/// Three failure modes the caller surfaces verbatim:
///   - Local migrator missing a version present in the archive
///     → the archive is from a *newer* build than the one importing it,
///     and we genuinely can't roll the schema forward.
///   - A version both sides have whose stored checksum is not that of
///     our SQL, in either line-ending form → the archive's build applied
///     different SQL under the same migration (one amended before a
///     release, a fork), and its schema is not the one ours expects.
///   - Anything else → propagated as a generic `Other` error.
///
/// Only the line-ending case is rewritten, the one
/// [`crate::db::migration_heal`] repairs on every open: the same SQL,
/// checked out with CRLF by an older Windows build. Every row is checked
/// before the first is rewritten, so a refused archive is left as it
/// came.
async fn normalise_migration_checksums(db_path: &Path) -> AppResult<()> {
    let migrator = sqlx::migrate!("../../migrations/profile");

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

    let verdict = judge_migrations(&migrator, &stored);
    // Closed before the caller removes the extracted profile: on Windows
    // an open `data.db` would keep its folder from going.
    let drifted = match verdict {
        MigrationVerdict::Compatible { drifted } => drifted,
        MigrationVerdict::NewerBuild { version } => {
            conn.close().await?;
            return Err(AppError::Other(format!(
                "archive contains migration {version} not present in this build — \
                 export was produced by a newer WaveFlow version"
            )));
        }
        MigrationVerdict::DifferentSql {
            version,
            description,
        } => {
            conn.close().await?;
            return Err(AppError::Other(format!(
                "archive was not imported: its migration {version} ({description}) was applied \
                 with different SQL than the one this build ships — export was produced by a \
                 different WaveFlow build"
            )));
        }
    };
    for (version, checksum) in drifted {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(checksum)
            .bind(version)
            .execute(&mut conn)
            .await?;
    }

    conn.close().await?;
    Ok(())
}

/// What this build makes of the migrations an archive's `data.db` records.
///
/// One judgement for the import and its preview, so the preview never
/// promises an import the import then refuses.
enum MigrationVerdict {
    /// Every recorded migration is one this build ships, with the same SQL.
    /// `drifted` lists the ones recorded under a CRLF checkout of that SQL,
    /// with the checksum the import rewrites them to.
    Compatible { drifted: Vec<(i64, Vec<u8>)> },
    /// The archive's build applied a migration this one does not have.
    NewerBuild { version: i64 },
    /// Both have the migration, but the archive's build applied other SQL
    /// under it (one amended before a release, a fork).
    DifferentSql { version: i64, description: String },
}

fn judge_migrations(
    migrator: &sqlx::migrate::Migrator,
    stored: &[(i64, Vec<u8>)],
) -> MigrationVerdict {
    let mut drifted = Vec::new();
    for (version, stored_checksum) in stored {
        let Some(local) = migrator.iter().find(|m| m.version == *version) else {
            return MigrationVerdict::NewerBuild { version: *version };
        };
        if local.checksum.as_ref() == stored_checksum.as_slice() {
            continue;
        }
        if !crate::db::migration_heal::is_line_ending_drift(local.sql.as_str(), stored_checksum) {
            return MigrationVerdict::DifferentSql {
                version: *version,
                description: local.description.to_string(),
            };
        }
        drifted.push((*version, local.checksum.to_vec()));
    }
    MigrationVerdict::Compatible { drifted }
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

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha384};
    use sqlx::sqlite::SqlitePoolOptions;

    /// An extracted archive's `data.db`, migrated by this build.
    async fn extracted(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("data.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        pool.close().await;
        path
    }

    /// Store `checksum` for `version`, as the archive's build would have.
    async fn forge(path: &Path, version: i64, checksum: &[u8]) {
        let mut conn = SqliteConnectOptions::new()
            .filename(path)
            .connect()
            .await
            .unwrap();
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(checksum)
            .bind(version)
            .execute(&mut conn)
            .await
            .unwrap();
        conn.close().await.unwrap();
    }

    async fn stored(path: &Path, version: i64) -> Vec<u8> {
        let mut conn = SqliteConnectOptions::new()
            .filename(path)
            .connect()
            .await
            .unwrap();
        let checksum =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(version)
                .fetch_one(&mut conn)
                .await
                .unwrap();
        conn.close().await.unwrap();
        checksum
    }

    fn sha384(bytes: &[u8]) -> Vec<u8> {
        Sha384::digest(bytes).to_vec()
    }

    #[tokio::test]
    async fn a_migration_checked_out_with_crlf_is_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = extracted(dir.path()).await;
        let migrator = sqlx::migrate!("../../migrations/profile");
        let first = migrator.iter().next().unwrap();
        let crlf = first
            .sql
            .as_str()
            .replace("\r\n", "\n")
            .replace('\n', "\r\n");
        forge(&path, first.version, &sha384(crlf.as_bytes())).await;

        normalise_migration_checksums(&path).await.unwrap();
        assert_eq!(stored(&path, first.version).await, first.checksum.as_ref());
    }

    #[tokio::test]
    async fn a_migration_whose_sql_changed_refuses_the_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = extracted(dir.path()).await;
        let migrator = sqlx::migrate!("../../migrations/profile");
        let mut versions = migrator.iter();
        let drifted = versions.next().unwrap();
        let changed = versions.next().unwrap();
        let crlf = drifted
            .sql
            .as_str()
            .replace("\r\n", "\n")
            .replace('\n', "\r\n");
        forge(&path, drifted.version, &sha384(crlf.as_bytes())).await;
        let edited = format!(
            "{}\nALTER TABLE track ADD COLUMN extra TEXT;\n",
            changed.sql.as_str()
        );
        let forged = sha384(edited.as_bytes());
        forge(&path, changed.version, &forged).await;

        let err = normalise_migration_checksums(&path)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(&changed.version.to_string()), "{err}");
        assert!(err.contains(changed.description.as_ref()), "{err}");
        // Nothing rewritten, the line-ending row included.
        assert_eq!(stored(&path, changed.version).await, forged);
        assert_eq!(
            stored(&path, drifted.version).await,
            sha384(crlf.as_bytes())
        );
    }

    // ── preview ────────────────────────────────────────────────────

    async fn exec(path: &Path, sql: &'static str) {
        let mut conn = SqliteConnectOptions::new()
            .filename(path)
            .connect()
            .await
            .unwrap();
        sqlx::raw_sql(sql).execute(&mut conn).await.unwrap();
        conn.close().await.unwrap();
    }

    async fn count(path: &Path, sql: &'static str) -> i64 {
        let mut conn = SqliteConnectOptions::new()
            .filename(path)
            .connect()
            .await
            .unwrap();
        let n = sqlx::query_scalar(sql).fetch_one(&mut conn).await.unwrap();
        conn.close().await.unwrap();
        n
    }

    fn manifest(archive_version: u32) -> ArchiveManifest {
        ArchiveManifest {
            archive_version,
            app_version: "9.9.9".into(),
            profile_name: "Weekend".into(),
            source_profile_id: 3,
            exported_at: "2026-10-25T12:00:00+00:00".into(),
        }
    }

    /// A profile directory with a migrated `data.db` holding two playlists,
    /// one smart playlist and one cover, archived the way an export does.
    async fn profile_archive(dir: &Path, archive_version: u32) -> (PathBuf, PathBuf) {
        let profile = dir.join("profile");
        std::fs::create_dir_all(profile.join("artwork")).unwrap();
        std::fs::write(profile.join("artwork").join("cover.jpg"), b"jpeg").unwrap();
        let db = extracted(&profile).await;
        exec(
            &db,
            "INSERT INTO playlist (name, is_smart, created_at, updated_at) VALUES
                ('A', 0, 1, 1), ('B', 0, 1, 1), ('Smart', 1, 1, 1);",
        )
        .await;
        let target = dir.join("weekend.waveflow");
        write_archive(
            &target,
            &profile,
            &db,
            &profile.join("artwork"),
            None,
            &manifest(archive_version),
        )
        .unwrap();
        (target, db)
    }

    #[tokio::test]
    async fn a_preview_says_what_an_archive_holds() {
        let dir = tempfile::tempdir().unwrap();
        let (archive, db) = profile_archive(dir.path(), ARCHIVE_VERSION).await;

        let preview = preview_archive(&archive).await.unwrap();
        assert_eq!(preview.compatibility, ArchiveCompatibility::Compatible);
        assert_eq!(preview.profile_name, "Weekend");
        assert_eq!(preview.app_version, "9.9.9");
        assert_eq!(preview.artwork_files, 1);
        assert_eq!(preview.metadata_artwork_files, 0);
        assert_eq!(preview.schema_version, Some(preview.local_schema_version));
        assert!(preview.size_bytes > 0);
        let contents = preview.contents.unwrap();
        assert_eq!(contents.playlists, 2);
        assert_eq!(contents.smart_playlists, 1);
        assert_eq!(contents.tracks, 0);
        assert_eq!(
            contents.settings,
            count(&db, "SELECT count(*) FROM profile_setting").await
        );
    }

    /// The preview refuses what the import refuses, for the same reason.
    #[tokio::test]
    async fn a_preview_flags_an_archive_from_a_newer_build() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let db = extracted(&profile).await;
        exec(
            &db,
            "INSERT INTO _sqlx_migrations
                (version, description, installed_on, success, checksum, execution_time)
             VALUES (99991231235959, 'from the future', '2099-01-01', 1, x'00', 0);",
        )
        .await;
        let archive = dir.path().join("future.waveflow");
        write_archive(
            &archive,
            &profile,
            &db,
            &profile.join("artwork"),
            None,
            &manifest(ARCHIVE_VERSION),
        )
        .unwrap();

        let preview = preview_archive(&archive).await.unwrap();
        assert_eq!(
            preview.compatibility,
            ArchiveCompatibility::NewerSchema {
                version: 99991231235959
            }
        );
        assert_eq!(preview.schema_version, Some(99991231235959));
        assert!(preview.contents.is_some(), "the counts are still shown");
    }

    #[tokio::test]
    async fn a_preview_flags_an_archive_from_a_different_build() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let db = extracted(&profile).await;
        let migrator = sqlx::migrate!("../../migrations/profile");
        let changed = migrator.iter().nth(1).unwrap();
        forge(&db, changed.version, &sha384(b"other sql")).await;
        let archive = dir.path().join("fork.waveflow");
        write_archive(
            &archive,
            &profile,
            &db,
            &profile.join("artwork"),
            None,
            &manifest(ARCHIVE_VERSION),
        )
        .unwrap();

        let preview = preview_archive(&archive).await.unwrap();
        assert_eq!(
            preview.compatibility,
            ArchiveCompatibility::DifferentBuild {
                version: changed.version
            }
        );
        // And the import, handed the same database, refuses it too.
        assert!(normalise_migration_checksums(&db).await.is_err());
    }

    #[tokio::test]
    async fn a_preview_does_not_open_a_newer_archive_layout() {
        let dir = tempfile::tempdir().unwrap();
        let (archive, _) = profile_archive(dir.path(), ARCHIVE_VERSION + 1).await;
        let preview = preview_archive(&archive).await.unwrap();
        assert_eq!(preview.compatibility, ArchiveCompatibility::NewerArchive);
        assert!(preview.contents.is_none());
        assert_eq!(preview.profile_name, "Weekend");
    }

    fn assert_unreadable(result: AppResult<ArchivePreview>) {
        let err = result.unwrap_err().to_string();
        assert!(err.contains(ERR_UNREADABLE), "{err}");
    }

    #[tokio::test]
    async fn a_file_that_is_not_an_archive_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.waveflow");
        std::fs::write(&path, b"\xff\xd8\xff not a zip at all").unwrap();
        assert_unreadable(preview_archive(&path).await);
    }

    #[tokio::test]
    async fn a_truncated_archive_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let (archive, _) = profile_archive(dir.path(), ARCHIVE_VERSION).await;
        let bytes = std::fs::read(&archive).unwrap();
        std::fs::write(&archive, &bytes[..bytes.len() / 2]).unwrap();
        assert_unreadable(preview_archive(&archive).await);
    }

    #[tokio::test]
    async fn an_archive_whose_database_is_damaged_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let db = extracted(&profile).await;
        // Keep the header, so SQLite opens it, and break the pages after it.
        let mut bytes = std::fs::read(&db).unwrap();
        for byte in bytes.iter_mut().skip(100) {
            *byte = 0xA5;
        }
        std::fs::write(&db, &bytes).unwrap();
        let archive = dir.path().join("damaged.waveflow");
        write_archive(
            &archive,
            &profile,
            &db,
            &profile.join("artwork"),
            None,
            &manifest(ARCHIVE_VERSION),
        )
        .unwrap();
        assert_unreadable(preview_archive(&archive).await);
    }

    #[test]
    fn a_scratch_file_is_removed_when_dropped() {
        let scratch = ScratchFile::new("db");
        std::fs::write(&scratch.path, b"x").unwrap();
        let path = scratch.path.clone();
        drop(scratch);
        assert!(!path.exists());
    }
}
