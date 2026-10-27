use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

/// The real migrator, so the columns and the triggers under test are
/// the ones the app ships.
async fn pool() -> SqlitePool {
    let options = SqliteConnectOptions::from_str(":memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn insert_track(pool: &SqlitePool, id: i64, title: &str, pinyin: Option<&str>) {
    sqlx::query(
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, duration_ms, added_at, is_available, pinyin,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (?, 1, ?, 'h', 1, 0, ?, 0, 0, 1, ?, 0, 0, 0, 0)",
    )
    .bind(id)
    .bind(format!("/m/{id}.flac"))
    .bind(title)
    .bind(pinyin)
    .execute(pool)
    .await
    .unwrap();
}

async fn pinyin_of(pool: &SqlitePool, id: i64) -> Option<String> {
    sqlx::query_scalar("SELECT pinyin FROM track WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_pass_fills_han_and_marks_the_rest() {
    let pool = pool().await;
    insert_track(&pool, 1, "中国人", None).await;
    insert_track(&pool, 2, "Dark Side", None).await;

    assert_eq!(fill_table(&pool, "track", "title").await.unwrap(), 2);
    assert_eq!(
        pinyin_of(&pool, 1).await.as_deref(),
        Some("zhongguoren zgr")
    );
    // The marker: looked at, nothing to romanise. Left NULL, the next
    // launch would read this row again, and every launch after that.
    assert_eq!(pinyin_of(&pool, 2).await.as_deref(), Some(""));

    // And a second pass finds nothing, which is what makes it a
    // once-per-database cost rather than a startup tax.
    assert_eq!(fill_table(&pool, "track", "title").await.unwrap(), 0);
}

#[tokio::test]
async fn a_value_written_meanwhile_is_not_overwritten() {
    let pool = pool().await;
    insert_track(&pool, 1, "中国人", None).await;

    // The gap this guards: the pass has read the row as NULL and is
    // computing, and a scan or a tag edit writes the blob for a
    // title it never saw. Played in that order, with the statement
    // the pass itself uses -- an unguarded write would replace the
    // fresher value with the stale one.
    sqlx::query("UPDATE track SET title = '稻香', pinyin = 'daoxiang dx' WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let written = write_batch(
        &pool,
        &update_sql("track"),
        &[(1, "zhongguoren zgr".to_string())],
    )
    .await
    .unwrap();

    // Zero, and reported as zero: the count comes back from the
    // database, so a skipped row cannot inflate the log line.
    assert_eq!(written, 0, "the row no longer qualifies");
    assert_eq!(pinyin_of(&pool, 1).await.as_deref(), Some("daoxiang dx"));
}

#[test]
fn the_marker_tells_computed_from_uncomputed() {
    // The whole reason the empty string is written rather than left
    // NULL: without it a Latin library re-reads every row on every
    // launch, because "no pinyin" and "not looked at yet" would be
    // the same value.
    assert_eq!(pinyin_blob("Dark Side"), None);
    let stored = pinyin_blob("Dark Side").unwrap_or_default();
    assert_eq!(stored, "");
}

#[test]
fn every_table_carries_the_column_it_is_swept_on() {
    // The pairs are the ones the migration added `pinyin` to.
    assert_eq!(TABLES.len(), 3);
    assert!(TABLES.iter().any(|(t, c)| *t == "track" && *c == "title"));
    assert!(TABLES.iter().any(|(t, c)| *t == "artist" && *c == "name"));
}
