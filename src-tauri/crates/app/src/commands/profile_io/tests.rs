use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha384};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{ConnectOptions, Connection};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use super::*;
use crate::error::AppResult;

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
    let checksum = sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
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
    // In WAL, as a profile database is: its header carries the mode.
    exec(
        &db,
        "PRAGMA journal_mode = WAL;
             INSERT INTO playlist (name, is_smart, created_at, updated_at) VALUES
                ('A', 0, 1, 1), ('B', 0, 1, 1), ('Smart', 1, 1, 1);
             PRAGMA wal_checkpoint(TRUNCATE);",
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
    assert_eq!(preview.artwork_files, Some(1));
    assert_eq!(preview.metadata_artwork_files, Some(0));
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
    // Nothing left beside the scratch copy of a WAL database.
    let leftovers: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.starts_with("waveflow-preview-") && (n.ends_with("-wal") || n.ends_with("-shm"))
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
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
    assert_eq!(preview.artwork_files, None);
    assert_eq!(preview.profile_name, "Weekend");
}

/// A newer layout is described as one even when it has no `data.db`
/// where this build would look: the manifest is read first.
#[tokio::test]
async fn a_newer_layout_without_a_database_is_still_newer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("next.waveflow");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    zip.start_file(MANIFEST_FILENAME, SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&serde_json::to_vec(&manifest(ARCHIVE_VERSION + 1)).unwrap())
        .unwrap();
    zip.start_file("profile.sqlite", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"whatever comes next").unwrap();
    zip.finish().unwrap();

    let preview = preview_archive(&path).await.unwrap();
    assert_eq!(preview.compatibility, ArchiveCompatibility::NewerArchive);
}

#[test]
fn a_failed_write_of_the_copy_is_not_an_unreadable_archive() {
    let dir = tempfile::tempdir().unwrap();
    // A directory where the copy should go: the write side fails.
    let err = copy_entry(&mut std::io::Cursor::new(b"bytes"), dir.path())
        .unwrap_err()
        .to_string();
    assert!(!err.contains(ERR_UNREADABLE), "{err}");
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

/// A migrated `app.db` under `root`, as the app opens it.
async fn app_db(root: &Path) -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(root.join("app.db"))
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/app")
        .run(&pool)
        .await
        .unwrap();
    pool
}

#[tokio::test]
async fn a_failure_after_the_row_is_inserted_rolls_the_row_back() {
    let dir = tempfile::tempdir().unwrap();
    let (archive, _) = profile_archive(dir.path(), ARCHIVE_VERSION).await;
    let root = dir.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    let paths = crate::paths::AppPaths::from_root(root.clone(), None);
    let app_db = app_db(&root).await;
    // Records every insert, so the test knows the import got that far
    // rather than failing before it and passing for nothing.
    sqlx::raw_sql(
        "CREATE TABLE inserted (id INTEGER NOT NULL);
         CREATE TRIGGER record_insert AFTER INSERT ON profile
         BEGIN INSERT INTO inserted (id) VALUES (new.id); END;",
    )
    .execute(&app_db)
    .await
    .unwrap();
    // A file where the profiles directory goes: the row is inserted, then
    // laying out its directories fails.
    std::fs::write(&paths.profiles_dir, b"in the way").unwrap();

    let result = import_profile_inner(
        &app_db,
        &paths,
        archive.to_string_lossy().into_owned(),
        None,
    )
    .await;

    assert!(result.is_err(), "{result:?}");
    let inserted: i64 = sqlx::query_scalar("SELECT count(*) FROM inserted")
        .fetch_one(&app_db)
        .await
        .unwrap();
    assert_eq!(inserted, 1);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM profile")
        .fetch_one(&app_db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_failed_import_removes_the_profile_cache_moved_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let (archive, _) = profile_archive(dir.path(), ARCHIVE_VERSION).await;
    let root = dir.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    let paths = crate::paths::AppPaths::from_root(root.clone(), None)
        .with_cache_root(dir.path().join("cache"));
    let app_db = app_db(&root).await;
    sqlx::raw_sql(
        "CREATE TABLE inserted (id INTEGER NOT NULL);
         CREATE TRIGGER record_insert AFTER INSERT ON profile
         BEGIN INSERT INTO inserted (id) VALUES (new.id); END;",
    )
    .execute(&app_db)
    .await
    .unwrap();
    let next_id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) + 1 FROM profile")
        .fetch_one(&app_db)
        .await
        .unwrap();
    // A file where the profile's last cache directory goes: the profile
    // directory and most of its cache tree are laid out, then this fails.
    let blocker = paths.profile_remote_artwork_dir(next_id);
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, b"in the way").unwrap();

    let result = import_profile_inner(
        &app_db,
        &paths,
        archive.to_string_lossy().into_owned(),
        None,
    )
    .await;

    assert!(result.is_err(), "{result:?}");
    let inserted: i64 = sqlx::query_scalar("SELECT id FROM inserted")
        .fetch_one(&app_db)
        .await
        .unwrap();
    assert_eq!(inserted, next_id);
    assert!(!paths.profile_dir(next_id).exists());
    assert!(!paths.profile_cache_dir(next_id).exists());
}

#[tokio::test]
async fn a_wal_left_beside_a_closed_profile_is_folded_into_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("data.db");
    // A writer still holding the database, as a draining lease would: its
    // committed rows sit in the WAL, none of them in the main file yet.
    let mut writer = SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .connect()
        .await
        .unwrap();
    sqlx::raw_sql(
        "PRAGMA wal_autocheckpoint = 0;
         CREATE TABLE t (v INTEGER);
         INSERT INTO t VALUES (1), (2), (3);",
    )
    .execute(&mut writer)
    .await
    .unwrap();

    checkpoint_wal_at(&db).await.unwrap();

    // What the archive copies: the main file alone, without its WAL.
    let copy = dir.path().join("copy.db");
    std::fs::copy(&db, &copy).unwrap();
    let mut reader = SqliteConnectOptions::new()
        .filename(&copy)
        .immutable(true)
        .connect()
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM t")
        .fetch_one(&mut reader)
        .await
        .unwrap();
    assert_eq!(rows, 3);
    let _ = reader.close().await;
    let _ = writer.close().await;
}

#[tokio::test]
async fn a_closed_profile_without_a_wal_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("data.db");
    checkpoint_wal_at(&db).await.unwrap();
    assert!(!db.exists(), "no WAL, so nothing is opened or created");
}

#[tokio::test]
async fn a_checkpoint_a_reader_blocks_fails_rather_than_pass() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("data.db");
    let mut writer = SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .connect()
        .await
        .unwrap();
    sqlx::raw_sql(
        "PRAGMA wal_autocheckpoint = 0;
         CREATE TABLE t (v INTEGER);
         INSERT INTO t VALUES (1);",
    )
    .execute(&mut writer)
    .await
    .unwrap();
    // A reader holding an older snapshot, then pages written after it:
    // those cannot be folded back while the reader is open.
    let mut reader = SqliteConnectOptions::new()
        .filename(&db)
        .connect()
        .await
        .unwrap();
    sqlx::raw_sql("BEGIN; SELECT count(*) FROM t;")
        .execute(&mut reader)
        .await
        .unwrap();
    sqlx::query("INSERT INTO t VALUES (2)")
        .execute(&mut writer)
        .await
        .unwrap();

    assert!(checkpoint_wal_at(&db).await.is_err());

    let _ = reader.close().await;
    let _ = writer.close().await;
}
