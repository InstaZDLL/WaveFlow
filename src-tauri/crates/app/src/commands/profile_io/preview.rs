//! Previewing an archive before it is imported: its contents and whether
//! this build can open it.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use zip::ZipArchive;

use crate::error::{AppError, AppResult};

use super::*;

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
    /// Entries under `artwork/` and `metadata_artwork/`; `None`, like
    /// `contents`, for a layout this build does not read.
    pub artwork_files: Option<u64>,
    pub metadata_artwork_files: Option<u64>,
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

pub(super) async fn preview_archive(source: &Path) -> AppResult<ArchivePreview> {
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
pub(super) struct ScratchFile {
    pub(super) path: PathBuf,
}

impl ScratchFile {
    pub(super) fn new(extension: &str) -> Self {
        let name = format!("waveflow-preview-{}.{extension}", uuid::Uuid::new_v4());
        Self {
            path: std::env::temp_dir().join(name),
        }
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        // The database is opened `immutable` and creates none of these;
        // removing them anyway keeps a later change of that from leaving
        // files in a directory nobody empties on Windows.
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut side = self.path.clone().into_os_string();
            side.push(suffix);
            let _ = std::fs::remove_file(side);
        }
    }
}

/// What the zip itself says, before its database is opened.
struct ArchiveInventory {
    manifest: ArchiveManifest,
    /// `None` for a layout this build does not read: its entries are left
    /// alone, and no `data.db` was copied.
    artwork_files: Option<u64>,
    metadata_artwork_files: Option<u64>,
    size_bytes: u64,
}

/// Read the manifest, then — for the one layout this build knows — count
/// the artwork entries and copy `data.db` to `db_out`. A failure to read
/// any of those is the archive being unreadable.
///
/// The manifest comes first so that an archive from a newer build is
/// described as one even if its layout moved or renamed `data.db`, or
/// compressed an entry in a way this build cannot read.
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
    if manifest.archive_version != ARCHIVE_VERSION {
        return Ok(ArchiveInventory {
            manifest,
            artwork_files: None,
            metadata_artwork_files: None,
            size_bytes,
        });
    }

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
            copy_entry(&mut entry, db_out)?;
            has_db = true;
        }
    }
    if !has_db {
        return Err(unreadable("no data.db"));
    }
    Ok(ArchiveInventory {
        manifest,
        artwork_files: Some(artwork_files),
        metadata_artwork_files: Some(metadata_artwork_files),
        size_bytes,
    })
}

/// Copy a zip entry to `dest`, telling the two ends apart: a failed read
/// is the archive being unreadable, a failed write (a full temporary
/// directory) is not, and must not be reported as a damaged archive.
pub(super) fn copy_entry(entry: &mut impl Read, dest: &Path) -> AppResult<()> {
    let scratch = |e: std::io::Error| AppError::Other(format!("preview scratch copy: {e}"));
    let mut out = File::create(dest).map_err(scratch)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match entry.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(unreadable(format!("data.db: {e}"))),
        };
        out.write_all(&buf[..n]).map_err(scratch)?;
    }
}

/// The archive's database, as far as a preview needs it.
struct ArchiveDb {
    contents: ArchiveContents,
    migrations: Vec<(i64, Vec<u8>)>,
}

/// Open the copied database, check it, and count what it holds.
///
/// Opened `immutable`, as the pre-migration copy is verified: nothing else
/// knows the file, and a profile database is in WAL mode, so a plain
/// read-only open would leave a `-wal` and a `-shm` beside the scratch file
/// in the temporary directory.
async fn read_archive_db(path: &Path) -> AppResult<ArchiveDb> {
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .immutable(true)
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
