use super::*;

async fn album_year(pool: &SqlitePool, id: i64) -> Option<i64> {
    sqlx::query_scalar("SELECT year FROM album WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn fixture_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE album (id INTEGER PRIMARY KEY, year INTEGER);
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 album_id INTEGER,
                 folder_id INTEGER NOT NULL,
                 file_path TEXT NOT NULL,
                 disc_number INTEGER,
                 track_number INTEGER,
                 year INTEGER,
                 is_available INTEGER NOT NULL DEFAULT 1
             );",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test]
async fn rescan_picks_a_stable_year_and_applies_tag_corrections() {
    let pool = fixture_pool().await;
    sqlx::raw_sql(
        "INSERT INTO album (id, year)
             VALUES (1, NULL), (2, 1999), (3, NULL), (4, NULL);
             INSERT INTO track
                 (id, album_id, folder_id, file_path, disc_number, track_number, year)
             VALUES (2, 1, 1, 'a-track2.mp3', 1, 2, 2025),
                    (1, 1, 1, 'z-track1.mp3', 1, 1, 2024),
                    (3, 2, 2, 'other.mp3', 1, 1, 2000),
                    (4, 3, 1, 'a-disc2.mp3', 2, 1, 2030),
                    (5, 3, 1, 'z-disc1.mp3', 1, 2, 2020),
                    (6, 4, 1, 'z-tie.mp3', 1, 1, 2040),
                    (7, 4, 1, 'a-tie.mp3', 1, 1, 2035);",
    )
    .execute(&pool)
    .await
    .unwrap();
    let repo = SqliteAlbumRepository::new(pool.clone());

    assert_eq!(
        repo.refresh_years_after_folder_scan(1, &[]).await.unwrap(),
        3
    );
    // Each winner conflicts with the next sort key (or the row id),
    // so removing disc, track, or path ordering changes one result.
    assert_eq!(album_year(&pool, 1).await, Some(2024));
    assert_eq!(album_year(&pool, 2).await, Some(1999));
    assert_eq!(album_year(&pool, 3).await, Some(2020));
    assert_eq!(album_year(&pool, 4).await, Some(2035));

    sqlx::query("UPDATE track SET year = 2026 WHERE album_id = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repo.refresh_years_after_folder_scan(1, &[]).await.unwrap(),
        1
    );
    assert_eq!(album_year(&pool, 1).await, Some(2026));

    sqlx::query("UPDATE track SET year = NULL WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repo.refresh_years_after_folder_scan(1, &[]).await.unwrap(),
        0
    );
    assert_eq!(album_year(&pool, 1).await, Some(2026));

    sqlx::query("UPDATE track SET year = NULL WHERE id = 2")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repo.refresh_years_after_folder_scan(1, &[]).await.unwrap(),
        1
    );
    assert_eq!(album_year(&pool, 1).await, None);
}

#[tokio::test]
async fn reassignment_updates_the_former_album_in_another_folder() {
    let pool = fixture_pool().await;
    sqlx::raw_sql(
        "INSERT INTO album (id, year) VALUES (1, 2024), (2, 2025);
             INSERT INTO track (id, album_id, folder_id, file_path, track_number, year)
             VALUES (1, 1, 2, 'moved.mp3', 1, 2024),
                    (2, 1, 1, 'remaining.mp3', 2, 2020),
                    (3, 2, 2, 'other.mp3', 2, 2025);",
    )
    .execute(&pool)
    .await
    .unwrap();
    let repo = SqliteAlbumRepository::new(pool.clone());

    // Retagging a file in folder 2 moves it to album 2. Album 1
    // still has a track in folder 1, but none in the scanned folder.
    sqlx::query("UPDATE track SET album_id = 2 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repo.refresh_years_after_folder_scan(2, &[1]).await.unwrap(),
        2
    );
    assert_eq!(album_year(&pool, 1).await, Some(2020));
    assert_eq!(album_year(&pool, 2).await, Some(2024));
}
