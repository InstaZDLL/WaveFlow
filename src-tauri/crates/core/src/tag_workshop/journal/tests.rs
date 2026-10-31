use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// The real profile schema, foreign keys on: a constraint the migration
/// declares is one these tests meet.
async fn pool() -> SqlitePool {
    let opts = SqliteConnectOptions::from_str(":memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0)")
        .execute(&pool)
        .await
        .unwrap();
    for id in [1, 2] {
        sqlx::query(
            "INSERT INTO track
                (id, library_id, file_path, file_hash, file_size, file_modified,
                 title, duration_ms, added_at)
             VALUES (?, 1, ?, 'h', 0, 0, 'T', 1000, 0)",
        )
        .bind(id)
        .bind(format!("/m/{id}.flac"))
        .execute(&pool)
        .await
        .unwrap();
    }
    pool
}

fn diffs() -> Vec<FieldDiff> {
    vec![
        FieldDiff {
            field: Field::Title,
            old: "smells like teen spirit".into(),
            new: "Smells Like Teen Spirit".into(),
        },
        FieldDiff {
            field: Field::MbReleaseId,
            old: String::new(),
            new: "9dce399d-3ec7-34a4-97d0-b4bac54e5f08".into(),
        },
    ]
}

#[tokio::test]
async fn a_written_file_keeps_its_old_values_and_its_hash() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let batch = create_batch(&mut conn, "Nevermind", Some("9dce"), 10)
        .await
        .unwrap();
    let file = journal_file(&mut conn, batch, Some(1), "/m/1.flac", &diffs())
        .await
        .unwrap();

    // Journaled before the write: pending, and nothing to undo yet.
    let files = batch_files(&mut conn, batch).await.unwrap();
    assert_eq!(files[0].status, FileStatus::Pending);
    assert_eq!(files[0].hash_after, None);

    mark_written(&mut conn, file, "abc").await.unwrap();
    let files = batch_files(&mut conn, batch).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].status, FileStatus::Written);
    assert_eq!(files[0].hash_after.as_deref(), Some("abc"));
    assert_eq!(
        files[0].fields,
        vec![
            JournaledField {
                field: Field::Title,
                old: Some("smells like teen spirit".into()),
                new: "Smells Like Teen Spirit".into(),
            },
            // A field the file did not carry is recorded as absent, so
            // the undo removes it rather than writing an empty value.
            JournaledField {
                field: Field::MbReleaseId,
                old: None,
                new: "9dce399d-3ec7-34a4-97d0-b4bac54e5f08".into(),
            },
        ]
    );

    let listed = batches(&mut conn, 10).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].label, "Nevermind");
    assert_eq!((listed[0].files, listed[0].written), (1, 1));
    assert_eq!(listed[0].undone_at, None);
}

#[tokio::test]
async fn a_batch_is_undone_only_once_no_file_is_left_written() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let batch = create_batch(&mut conn, "b", None, 10).await.unwrap();
    let one = journal_file(&mut conn, batch, Some(1), "/m/1.flac", &diffs())
        .await
        .unwrap();
    let two = journal_file(&mut conn, batch, Some(2), "/m/2.flac", &diffs())
        .await
        .unwrap();
    mark_written(&mut conn, one, "h1").await.unwrap();
    mark_written(&mut conn, two, "h2").await.unwrap();

    // One file restored, the other refused (changed since): still open.
    mark_undone(&mut conn, one).await.unwrap();
    assert!(!settle_undo(&mut conn, batch, 20).await.unwrap());
    assert_eq!(batches(&mut conn, 10).await.unwrap()[0].undone_at, None);

    mark_undone(&mut conn, two).await.unwrap();
    assert!(settle_undo(&mut conn, batch, 30).await.unwrap());
    let listed = batches(&mut conn, 10).await.unwrap();
    assert_eq!(listed[0].undone_at, Some(30));
    assert_eq!(listed[0].undone, 2);
}

#[tokio::test]
async fn a_batch_that_wrote_nothing_is_dropped_and_one_that_did_is_kept() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let empty = create_batch(&mut conn, "refused", None, 10).await.unwrap();
    let file = journal_file(&mut conn, empty, Some(1), "/m/1.flac", &diffs())
        .await
        .unwrap();
    mark_failed(&mut conn, file, "read-only").await.unwrap();
    assert!(discard_if_empty(&mut conn, empty).await.unwrap());
    // The cascade takes its files and fields with it.
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tag_workshop_field")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(left, 0);

    let kept = create_batch(&mut conn, "kept", None, 10).await.unwrap();
    let file = journal_file(&mut conn, kept, Some(1), "/m/1.flac", &diffs())
        .await
        .unwrap();
    mark_written(&mut conn, file, "h").await.unwrap();
    assert!(!discard_if_empty(&mut conn, kept).await.unwrap());
    assert_eq!(batches(&mut conn, 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_track_removed_from_the_library_keeps_its_journal() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let batch = create_batch(&mut conn, "b", None, 10).await.unwrap();
    let file = journal_file(&mut conn, batch, Some(1), "/m/1.flac", &diffs())
        .await
        .unwrap();
    mark_written(&mut conn, file, "h").await.unwrap();
    sqlx::query("DELETE FROM track WHERE id = 1")
        .execute(&mut *conn)
        .await
        .unwrap();
    let files = batch_files(&mut conn, batch).await.unwrap();
    assert_eq!(files[0].track_id, None);
    assert_eq!(files[0].file_path, "/m/1.flac");
    assert_eq!(files[0].fields.len(), 2);
}
