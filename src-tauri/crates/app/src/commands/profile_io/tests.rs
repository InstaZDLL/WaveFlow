use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha384};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{ConnectOptions, Connection, SqlitePool};
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

    // Written by hand: an export now refuses such a database, see below, and
    // an archive like this comes from elsewhere, or from an older build.
    let archive = dir.path().join("damaged.waveflow");
    let mut zip = ZipWriter::new(File::create(&archive).unwrap());
    let options = SimpleFileOptions::default();
    zip.start_file(MANIFEST_FILENAME, options).unwrap();
    zip.write_all(&serde_json::to_vec(&manifest(ARCHIVE_VERSION)).unwrap())
        .unwrap();
    zip.start_file("data.db", options).unwrap();
    zip.write_all(&bytes).unwrap();
    zip.finish().unwrap();
    assert_unreadable(preview_archive(&archive).await);

    // A database that cannot be read cannot be checked for a share's password,
    // so it does not go into an archive on the chance that it holds none. The
    // file is still there for whoever wants to rescue it, and a backup that
    // fails keeps the last good one.
    let refused = dir.path().join("refused.waveflow");
    assert!(write_archive(
        &refused,
        &profile,
        &db,
        &profile.join("artwork"),
        None,
        &manifest(ARCHIVE_VERSION),
    )
    .is_err());
    assert!(!refused.exists());
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

// ── a share's password ─────────────────────────────────────────

/// A profile with a share's password, exported by the real `write_archive`:
/// the row and its bytes must be gone from the archive, in clear or anywhere
/// in a free page, and the database at home must keep it.
#[tokio::test(flavor = "multi_thread")]
async fn an_export_leaves_a_shares_password_behind() {
    use std::io::Read;

    const MARKER: &str = "correct-horse-battery-staple-7f3a";
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(profile.join("artwork")).unwrap();
    let db = extracted(&profile).await;
    exec(
        &db,
        "PRAGMA journal_mode = WAL;
         INSERT INTO network_source (uuid, kind, name, endpoint, username, secret, created_at)
         VALUES ('u-1', 'webdav', 'NAS', 'https://nas.local/dav', 'me',
                 'correct-horse-battery-staple-7f3a', 1);
         PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .await;
    let at_home = std::fs::read(&db).unwrap();
    assert!(
        at_home
            .windows(MARKER.len())
            .any(|w| w == MARKER.as_bytes()),
        "the test only proves something if the bytes were there"
    );

    let target = dir.path().join("weekend.waveflow");
    let db_for_export = db.clone();
    let profile_for_export = profile.clone();
    let target_for_export = target.clone();
    tokio::task::spawn_blocking(move || {
        write_archive(
            &target_for_export,
            &profile_for_export,
            &db_for_export,
            &profile_for_export.join("artwork"),
            None,
            &manifest(ARCHIVE_VERSION),
        )
    })
    .await
    .unwrap()
    .unwrap();

    // Every entry of the archive, not only the database.
    let mut archive = zip::ZipArchive::new(File::open(&target).unwrap()).unwrap();
    let mut archived_db = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().unwrap().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert!(
            !bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
            "the password is in {name}"
        );
        if name == "data.db" {
            archived_db = bytes;
        }
    }
    assert!(!archived_db.is_empty());

    // The source itself travels: an imported profile asks for the password.
    let restored = dir.path().join("restored.db");
    std::fs::write(&restored, &archived_db).unwrap();
    assert_eq!(
        count(&restored, "SELECT COUNT(*) FROM network_source").await,
        1
    );
    assert_eq!(
        count(
            &restored,
            "SELECT COUNT(*) FROM network_source WHERE secret IS NULL"
        )
        .await,
        1
    );

    // The live database is untouched, and no working copy is left behind.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM network_source WHERE secret IS NOT NULL"
        )
        .await,
        1
    );
    let leftovers: Vec<_> = std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("export-"))
        .collect();
    assert!(leftovers.is_empty());
}

// ── a podcast's addresses ──────────────────────────────────────

/// A private feed's token, exported by the real `write_archive`: the addresses
/// are gone from the archive, in clear or in a free page, including ones written
/// over or removed before the export. The hashes stay, so subscribing again finds
/// the channel; the database at home keeps everything.
#[tokio::test(flavor = "multi_thread")]
async fn an_export_leaves_feed_addresses_behind() {
    use std::io::Read;

    const MARKER: &str = "f33d-t0k3n-a91c";
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(profile.join("artwork")).unwrap();
    let db = extracted(&profile).await;
    let at = |path: &str| format!("https://feeds.example.org/{MARKER}/{path}?auth={MARKER}");

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let feed = |episode: &str| waveflow_core::podcast::Feed {
        title: "Show".into(),
        description: None,
        author: None,
        language: None,
        site_url: Some(at("site")),
        image_url: Some(at("cover.png")),
        new_feed_url: None,
        episodes: vec![waveflow_core::podcast::FeedEpisode {
            guid: at(episode),
            title: "One".into(),
            description: None,
            published_at: None,
            duration_ms: None,
            season: None,
            episode_number: None,
            enclosure_url: at(episode),
            enclosure_type: None,
            enclosure_length: None,
            chapters_url: Some(at("chapters.json")),
            inline_chapters: vec![],
            transcripts: vec![waveflow_core::podcast::TranscriptRef {
                url: at("t.vtt"),
                kind: waveflow_core::podcast::TranscriptKind::Vtt,
                language: None,
            }],
        }],
    };
    let validators = crate::podcast::fetch::Validators {
        etag: Some(format!("\"{MARKER}\"")),
        last_modified: None,
    };
    // A channel kept, with an address written over at a later refresh, and one
    // removed: what is gone from the rows must be gone from the free pages too.
    crate::podcast::store::apply_feed(&pool, "kept", &at("feed"), &feed("old"), &validators)
        .await
        .unwrap();
    crate::podcast::store::apply_feed(&pool, "kept", &at("feed"), &feed("old"), &validators)
        .await
        .unwrap();
    let removed =
        crate::podcast::store::apply_feed(&pool, "gone", &at("gone"), &feed("gone"), &validators)
            .await
            .unwrap();
    assert!(crate::podcast::store::delete_channel(&pool, removed)
        .await
        .unwrap());
    sqlx::query("UPDATE podcast_episode SET position_ms = 4242")
        .execute(&pool)
        .await
        .unwrap();
    // A downloaded episode: its file stays here, in the folder of the caches, so
    // the row must not promise it where the archive lands.
    sqlx::query(
        "UPDATE podcast_episode
            SET download_file = '1.mp3', download_size = 4096, downloaded_at = 1",
    )
    .execute(&pool)
    .await
    .unwrap();
    std::fs::create_dir_all(profile.join("podcast-downloads")).unwrap();
    std::fs::write(profile.join("podcast-downloads/1.mp3"), MARKER).unwrap();
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES ('podcasts.enabled', 'true', 'bool', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let at_home = std::fs::read(&db).unwrap();
    assert!(
        at_home
            .windows(MARKER.len())
            .any(|w| w == MARKER.as_bytes()),
        "the test only proves something if the bytes were there"
    );

    let target = dir.path().join("weekend.waveflow");
    let db_for_export = db.clone();
    let profile_for_export = profile.clone();
    let target_for_export = target.clone();
    tokio::task::spawn_blocking(move || {
        write_archive(
            &target_for_export,
            &profile_for_export,
            &db_for_export,
            &profile_for_export.join("artwork"),
            None,
            &manifest(ARCHIVE_VERSION),
        )
    })
    .await
    .unwrap()
    .unwrap();

    let mut archive = zip::ZipArchive::new(File::open(&target).unwrap()).unwrap();
    let mut archived_db = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().unwrap().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert!(
            !bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
            "a feed address is in {name}"
        );
        if name == "data.db" {
            archived_db = bytes;
        }
    }
    assert!(!archived_db.is_empty());

    // The channel travels, and what it is known by, and the position.
    let restored = dir.path().join("restored.db");
    std::fs::write(&restored, &archived_db).unwrap();
    assert_eq!(
        count(
            &restored,
            "SELECT COUNT(*) FROM podcast_channel WHERE feed_key = 'kept' AND feed_url IS NULL
               AND site_url IS NULL AND image_url IS NULL AND etag IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &restored,
            "SELECT COUNT(*) FROM podcast_episode WHERE position_ms = 4242 AND enclosure_url IS NULL
               AND chapters_url IS NULL AND transcripts IS NULL
               AND download_file IS NULL AND download_size IS NULL AND downloaded_at IS NULL"
        )
        .await,
        1
    );
    // The warning does not travel with the channels.
    assert_eq!(
        count(
            &restored,
            "SELECT COUNT(*) FROM profile_setting WHERE key = 'podcasts.enabled'"
        )
        .await,
        0
    );

    // The live database is untouched.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM podcast_channel WHERE feed_url IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM profile_setting WHERE key = 'podcasts.enabled'"
        )
        .await,
        1
    );
}

/// An archive holding the podcast switch on, a channel naming the local network and
/// what an archive has no business vouching for, written by hand: `write_archive` would
/// leave all of it behind. `extra` is run over its database first.
async fn profile_archive_with_setting(dir: &Path, extra: &'static str) -> (PathBuf, PathBuf) {
    let profile = dir.join("profile");
    std::fs::create_dir_all(profile.join("artwork")).unwrap();
    let db = extracted(&profile).await;
    exec(
        &db,
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES ('podcasts.enabled', 'true', 'bool', 0);
         INSERT INTO podcast_channel (feed_key, feed_url, title, created_at, updated_at)
         VALUES ('k', 'http://192.168.1.1/admin', 'Router', 0, 0);
         INSERT INTO podcast_episode
            (channel_id, item_key, title, enclosure_url, download_file, download_size,
             downloaded_at, chapters_checked_at, chapters_cache, transcript_checked_at,
             transcript_kind, transcript_cache, position_ms, first_seen_at, updated_at)
         VALUES (1, 'i', 'One', 'http://192.168.1.1/a.mp3', '../../secrets.txt', 9, 1, 1,
                 '[]', 1, 'vtt', 'WEBVTT', 4242, 0, 0);",
    )
    .await;
    exec(&db, extra).await;
    let target = dir.join("handmade.waveflow");
    let mut zip = ZipWriter::new(File::create(&target).unwrap());
    let options = SimpleFileOptions::default();
    zip.start_file(MANIFEST_FILENAME, options).unwrap();
    zip.write_all(&serde_json::to_vec(&manifest(ARCHIVE_VERSION)).unwrap())
        .unwrap();
    zip.start_file("data.db", options).unwrap();
    zip.write_all(&std::fs::read(&db).unwrap()).unwrap();
    zip.finish().unwrap();
    (target, db)
}

/// An archive that says the switch is on, as one from another build or another
/// person could: the imported profile comes back off, so the warning shows again,
/// and the addresses it names are gone, so accepting that warning here does not make
/// the timer contact what somebody else wrote into the file.
#[tokio::test]
async fn an_imported_profile_comes_back_with_podcasts_off() {
    let dir = tempfile::tempdir().unwrap();
    let (archive, db_in_archive) = profile_archive_with_setting(dir.path(), "SELECT 1;").await;
    let root = dir.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    let paths = crate::paths::AppPaths::from_root(root.clone(), None);
    let app_db = app_db(&root).await;

    let imported = import_profile_inner(
        &app_db,
        &paths,
        archive.to_string_lossy().into_owned(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        count(
            &db_in_archive,
            "SELECT COUNT(*) FROM profile_setting WHERE key = 'podcasts.enabled'"
        )
        .await,
        1,
        "the test only proves something if the archive said so"
    );
    let pool = crate::db::profile_db::open(&paths.profile_db(imported.id), &paths.app_db)
        .await
        .unwrap();
    assert!(!crate::podcast::store::is_enabled(&pool).await.unwrap());
    let channels = crate::podcast::store::list_channels(&pool).await.unwrap();
    assert_eq!(channels.len(), 1, "the channel itself comes along");
    assert!(!channels[0].has_feed_address);
    assert!(crate::podcast::store::refreshable(&pool)
        .await
        .unwrap()
        .is_empty());
    // A path and a cache are not the archive's to provide. The position is the user's.
    let vouched: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), position_ms FROM podcast_episode
          WHERE enclosure_url IS NULL AND download_file IS NULL AND download_size IS NULL
            AND downloaded_at IS NULL AND chapters_checked_at IS NULL AND chapters_cache IS NULL
            AND transcript_checked_at IS NULL AND transcript_kind IS NULL
            AND transcript_cache IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(vouched, (1, 4242));
    pool.close().await;
}

/// A trigger brought by an archive runs on every later write, so it could put back what
/// the blanking removed. It goes first, along with a view, and a changed trigger of our
/// own is restored; the ones the migrations made stay.
#[tokio::test]
async fn an_imported_trigger_cannot_undo_the_blanking() {
    let dir = tempfile::tempdir().unwrap();
    let (archive, _) = profile_archive_with_setting(
        dir.path(),
        "CREATE TRIGGER evil_channel AFTER UPDATE ON podcast_channel BEGIN
            UPDATE podcast_channel SET feed_url = 'http://192.168.1.1/x' WHERE feed_url IS NULL;
         END;
         CREATE TRIGGER \"evil \"\"quoted\" AFTER DELETE ON profile_setting BEGIN
            INSERT INTO profile_setting (key, value, value_type, updated_at)
            VALUES ('podcasts.enabled', 'true', 'bool', 0);
         END;
         CREATE VIEW evil_view AS SELECT 1;",
    )
    .await;
    let root = dir.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    let paths = crate::paths::AppPaths::from_root(root.clone(), None);
    let app_db = app_db(&root).await;
    let imported = import_profile_inner(
        &app_db,
        &paths,
        archive.to_string_lossy().into_owned(),
        None,
    )
    .await
    .unwrap();
    let pool = crate::db::profile_db::open(&paths.profile_db(imported.id), &paths.app_db)
        .await
        .unwrap();
    assert!(!crate::podcast::store::is_enabled(&pool).await.unwrap());
    let channels = crate::podcast::store::list_channels(&pool).await.unwrap();
    assert!(!channels[0].has_feed_address);
    let names = |pool: SqlitePool| async move {
        let mut names: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type IN ('trigger', 'view') ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        names.sort();
        pool.close().await;
        names
    };
    let found = names(pool).await;
    assert!(!found.is_empty(), "the migrations' own triggers stay");
    assert!(
        found.iter().all(|(name,)| !name.starts_with("evil")),
        "{found:?}"
    );
    let clean = dir.path().join("clean");
    std::fs::create_dir_all(&clean).unwrap();
    let reference = crate::db::profile_db::open(&extracted(&clean).await, &paths.app_db)
        .await
        .unwrap();
    assert_eq!(found, names(reference).await);
}

/// One of our own triggers, changed or removed by an archive, is put back as the
/// migrations made it: `sqlx` believes they ran and would not run them again.
#[tokio::test]
async fn an_imported_profile_gets_our_own_triggers_back() {
    let dir = tempfile::tempdir().unwrap();
    let clean = dir.path().join("clean");
    std::fs::create_dir_all(&clean).unwrap();
    let db = extracted(&clean).await;
    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .connect()
        .await
        .unwrap();
    let expected: Vec<(String, String)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name")
            .fetch_all(&mut conn)
            .await
            .unwrap();
    let (first, second) = (&expected[0].0, &expected[1].0);
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP TRIGGER \"{first}\"")))
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP TRIGGER \"{second}\"")))
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER \"{second}\" AFTER INSERT ON profile_setting BEGIN SELECT 1; END"
    )))
    .execute(&mut conn)
    .await
    .unwrap();
    super::import::drop_schema_the_migrations_did_not_make(&mut conn)
        .await
        .unwrap();
    let after: Vec<(String, String)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name")
            .fetch_all(&mut conn)
            .await
            .unwrap();
    assert_eq!(after, expected);
    conn.close().await.unwrap();
}

/// Whatever keeps a value in place after the `UPDATE` is met by a refusal, not by a
/// report of success: here a trigger the blanking does not know about, on a database
/// that reached it without going through an import.
#[tokio::test]
async fn what_survives_the_blanking_refuses_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let clean = dir.path().join("clean");
    std::fs::create_dir_all(&clean).unwrap();
    let db = extracted(&clean).await;
    exec(
        &db,
        "INSERT INTO podcast_channel (feed_key, feed_url, title, created_at, updated_at)
         VALUES ('k', 'https://feeds.example.org/token', 'T', 0, 0);
         CREATE TRIGGER keep AFTER UPDATE ON podcast_channel BEGIN
            UPDATE podcast_channel SET feed_url = 'https://feeds.example.org/token'
             WHERE feed_url IS NULL;
         END;",
    )
    .await;
    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .connect()
        .await
        .unwrap();
    let refused = blank_what_does_not_travel(&mut conn).await;
    conn.close().await.unwrap();
    assert!(refused.is_err(), "{refused:?}");
}

/// The rebuild costs seconds on a large library, so it runs when it has something to
/// clear. Here it has not, and the old bytes of a playlist name are still in the file:
/// that is the rebuild not having run.
#[tokio::test(flavor = "multi_thread")]
async fn a_profile_that_never_held_a_channel_is_archived_without_a_rebuild() {
    use std::io::Read;

    const MARKER: &str = "n0t-a-s3cret-playlist-name-41";
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(profile.join("artwork")).unwrap();
    let db = extracted(&profile).await;
    exec(
        &db,
        "INSERT INTO playlist (name, is_smart, created_at, updated_at)
         VALUES ('n0t-a-s3cret-playlist-name-41', 0, 1, 1);
         DELETE FROM playlist;",
    )
    .await;
    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .connect()
        .await
        .unwrap();
    assert!(!has_ever_held_a_channel(&mut conn).await.unwrap());
    conn.close().await.unwrap();

    let target = dir.path().join("plain.waveflow");
    let (for_db, for_profile, for_target) = (db.clone(), profile.clone(), target.clone());
    tokio::task::spawn_blocking(move || {
        write_archive(
            &for_target,
            &for_profile,
            &for_db,
            &for_profile.join("artwork"),
            None,
            &manifest(ARCHIVE_VERSION),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let mut archive = zip::ZipArchive::new(File::open(&target).unwrap()).unwrap();
    let mut bytes = Vec::new();
    archive
        .by_name("data.db")
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()));
    // The copy is a usable database all the same, with no journal beside it.
    let leftovers: Vec<_> = std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("export-"))
        .collect();
    assert!(leftovers.is_empty());
    let restored = dir.path().join("restored.db");
    std::fs::write(&restored, &bytes).unwrap();
    assert_eq!(count(&restored, "SELECT COUNT(*) FROM playlist").await, 0);
}

/// Once a profile has held a channel, it is known for good, row or no row.
#[tokio::test]
async fn a_channel_once_held_is_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let clean = dir.path().join("clean");
    std::fs::create_dir_all(&clean).unwrap();
    let db = extracted(&clean).await;
    exec(
        &db,
        "INSERT INTO podcast_channel (feed_key, title, created_at, updated_at)
         VALUES ('k', 'T', 0, 0);
         DELETE FROM podcast_channel;",
    )
    .await;
    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .connect()
        .await
        .unwrap();
    assert!(has_ever_held_a_channel(&mut conn).await.unwrap());
    conn.close().await.unwrap();
}

/// With no channel left there is no row to blank, and the bytes are still in the
/// file: a removed row stays in a free page, and a row an ordinary write made grow
/// left its old self behind in the page. The archive holds none of them.
#[tokio::test(flavor = "multi_thread")]
async fn an_export_after_unsubscribing_holds_no_trace_of_the_address() {
    use std::io::Read;

    const MARKER: &str = "g0ne-t0k3n-77ab";
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(profile.join("artwork")).unwrap();
    let db = extracted(&profile).await;
    let at = |path: &str| format!("https://feeds.example.org/{MARKER}/{path}");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let episode = |enclosure: String| waveflow_core::podcast::FeedEpisode {
        guid: "the-guid".into(),
        title: "One".into(),
        description: None,
        published_at: None,
        duration_ms: None,
        season: None,
        episode_number: None,
        enclosure_url: enclosure,
        enclosure_type: None,
        enclosure_length: None,
        chapters_url: None,
        inline_chapters: vec![],
        transcripts: vec![],
    };
    let feed = |enclosure: String| waveflow_core::podcast::Feed {
        title: "Show".into(),
        description: None,
        author: None,
        language: None,
        site_url: None,
        image_url: None,
        new_feed_url: None,
        episodes: vec![episode(enclosure)],
    };
    let validators = crate::podcast::fetch::Validators::default();
    let id =
        crate::podcast::store::apply_feed(&pool, "k", &at("feed"), &feed(at("a")), &validators)
            .await
            .unwrap();
    // An error word makes the row grow, an address is written over, the feed moves,
    // and then the whole channel goes.
    crate::podcast::store::record_failure(&pool, id, "too_many_redirects")
        .await
        .unwrap();
    assert!(
        crate::podcast::store::refresh_feed(&pool, id, &feed(at("b")), &validators)
            .await
            .unwrap()
    );
    assert!(
        crate::podcast::store::move_channel(&pool, id, "k2", &at("moved"))
            .await
            .unwrap()
    );
    assert!(crate::podcast::store::delete_channel(&pool, id)
        .await
        .unwrap());
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(
        std::fs::read(&db)
            .unwrap()
            .windows(MARKER.len())
            .any(|w| w == MARKER.as_bytes()),
        "the test only proves something if the bytes were there"
    );

    let target = dir.path().join("after.waveflow");
    let db_for_export = db.clone();
    let profile_for_export = profile.clone();
    let target_for_export = target.clone();
    tokio::task::spawn_blocking(move || {
        write_archive(
            &target_for_export,
            &profile_for_export,
            &db_for_export,
            &profile_for_export.join("artwork"),
            None,
            &manifest(ARCHIVE_VERSION),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let mut archive = zip::ZipArchive::new(File::open(&target).unwrap()).unwrap();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().unwrap().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert!(
            !bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
            "a removed address is in {name}"
        );
    }
}

/// A small picture, as `set_profile_avatar` stores one.
fn a_picture() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(40, 30, image::Rgba([200, 30, 90, 255]));
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png).unwrap();
    waveflow_core::artwork::avatar::normalize_avatar(png.get_ref()).unwrap()
}

/// The picture travels: an export carries `avatar.png`, and the imported
/// profile comes back with it, stored under its own directory and named
/// in its row.
#[tokio::test]
async fn a_profile_picture_travels_with_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(profile.join("artwork")).unwrap();
    let db = extracted(&profile).await;
    let picture = a_picture();
    std::fs::write(profile.join(crate::paths::PROFILE_AVATAR_FILE), &picture).unwrap();
    let archive = dir.path().join("pictured.waveflow");
    write_archive(
        &archive,
        &profile,
        &db,
        &profile.join("artwork"),
        None,
        &manifest(ARCHIVE_VERSION),
    )
    .unwrap();
    assert_eq!(
        read_avatar_entry(&archive).unwrap().as_deref(),
        Some(picture.as_slice())
    );

    let root = dir.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    let paths = crate::paths::AppPaths::from_root(root.clone(), None);
    let app_db = app_db(&root).await;
    let imported = import_profile_inner(
        &app_db,
        &paths,
        archive.to_string_lossy().into_owned(),
        None,
    )
    .await
    .unwrap();

    let stored = std::fs::read(paths.profile_avatar(imported.id)).unwrap();
    let image = image::load_from_memory(&stored).unwrap();
    assert_eq!(image.width(), waveflow_core::artwork::avatar::AVATAR_SIZE);
    let hash: Option<String> = sqlx::query_scalar("SELECT avatar_hash FROM profile WHERE id = ?")
        .bind(imported.id)
        .fetch_one(&app_db)
        .await
        .unwrap();
    assert_eq!(hash, Some(blake3::hash(&stored).to_hex().to_string()));
}

/// An archive without a picture imports a profile without one.
#[tokio::test]
async fn an_archive_without_a_picture_imports_none() {
    let dir = tempfile::tempdir().unwrap();
    let (archive, _) = profile_archive(dir.path(), ARCHIVE_VERSION).await;
    assert!(read_avatar_entry(&archive).unwrap().is_none());
    let root = dir.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    let paths = crate::paths::AppPaths::from_root(root.clone(), None);
    let app_db = app_db(&root).await;
    let imported = import_profile_inner(
        &app_db,
        &paths,
        archive.to_string_lossy().into_owned(),
        None,
    )
    .await
    .unwrap();
    assert!(!paths.profile_avatar(imported.id).exists());
    let hash: Option<String> = sqlx::query_scalar("SELECT avatar_hash FROM profile WHERE id = ?")
        .bind(imported.id)
        .fetch_one(&app_db)
        .await
        .unwrap();
    assert_eq!(hash, None);
}

/// An `avatar.png` written by hand is no more trusted than a file on the
/// disk: one that is not a picture is left out, and the profile is
/// imported all the same.
#[tokio::test]
async fn a_handmade_picture_that_is_none_is_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let (archive, _) = profile_archive_with_setting(dir.path(), "SELECT 1;").await;
    // Append the entry to the handmade archive.
    let mut zip = ZipWriter::new_append(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&archive)
            .unwrap(),
    )
    .unwrap();
    zip.start_file(
        crate::paths::PROFILE_AVATAR_FILE,
        SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"\x89PNG\r\n\x1a\nthis is no picture")
        .unwrap();
    zip.finish().unwrap();

    let root = dir.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    let paths = crate::paths::AppPaths::from_root(root.clone(), None);
    let app_db = app_db(&root).await;
    let imported = import_profile_inner(
        &app_db,
        &paths,
        archive.to_string_lossy().into_owned(),
        None,
    )
    .await
    .unwrap();
    assert!(!paths.profile_avatar(imported.id).exists());
    let hash: Option<String> = sqlx::query_scalar("SELECT avatar_hash FROM profile WHERE id = ?")
        .bind(imported.id)
        .fetch_one(&app_db)
        .await
        .unwrap();
    assert_eq!(hash, None);
}
