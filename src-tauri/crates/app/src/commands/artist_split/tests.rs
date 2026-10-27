use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use waveflow_core::scanner::canonical_name;

/// In-memory profile DB migrated with the real schema (FKs on) so the
/// relink + cleanup guards are exercised against the same constraints
/// production uses — a stripped fixture would hide a broken FK guard.
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
    // One library so tracks satisfy the NOT NULL `library_id` FK.
    sqlx::query("INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0)")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

async fn seed_artist(pool: &SqlitePool, name: &str) -> i64 {
    sqlx::query("INSERT INTO artist (name, canonical_name) VALUES (?, ?)")
        .bind(name)
        .bind(canonical_name(name))
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
}

async fn seed_track(pool: &SqlitePool, id: i64, primary: Option<i64>) -> i64 {
    sqlx::query(
        "INSERT INTO track
                (id, library_id, file_path, file_hash, file_size, file_modified,
                 title, primary_artist, duration_ms, added_at)
             VALUES (?, 1, ?, 'h', 0, 0, 't', ?, 0, 0)",
    )
    .bind(id)
    .bind(format!("/f/{id}.flac"))
    .bind(primary)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn link(pool: &SqlitePool, track: i64, artist: i64, role: &str, position: i64) {
    sqlx::query(
        "INSERT INTO track_artist (track_id, artist_id, role, position) VALUES (?, ?, ?, ?)",
    )
    .bind(track)
    .bind(artist)
    .bind(role)
    .bind(position)
    .execute(pool)
    .await
    .unwrap();
}

async fn credits(pool: &SqlitePool, track: i64) -> Vec<(i64, String, i64)> {
    sqlx::query_as(
        "SELECT artist_id, role, position FROM track_artist
              WHERE track_id = ? ORDER BY position",
    )
    .bind(track)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn primary_of(pool: &SqlitePool, track: i64) -> Option<i64> {
    sqlx::query_scalar("SELECT primary_artist FROM track WHERE id = ?")
        .bind(track)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn artist_exists(pool: &SqlitePool, id: i64) -> bool {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artist WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    n == 1
}

#[tokio::test]
async fn splits_a_phantom_reusing_existing_rows() {
    let pool = pool().await;
    // Two of the three fragments already have enriched rows.
    let a = seed_artist(&pool, "Tibeauthetraveler").await;
    let b = seed_artist(&pool, "Nogymx").await;
    let phantom = seed_artist(&pool, "Tibeauthetraveler, Nogymx, Osaki").await;
    let t = seed_track(&pool, 10, Some(phantom)).await;
    link(&pool, t, phantom, "main", 0).await;

    let res = split_artist_inner(&pool, phantom).await.unwrap();

    // Existing rows reused; "Osaki" created fresh.
    assert_eq!(res.artists.len(), 3);
    assert_eq!(res.artists[0].id, a, "first fragment reuses existing row");
    assert_eq!(res.artists[1].id, b, "second fragment reuses existing row");
    let osaki = res.artists[2].id;
    assert!(osaki != phantom && osaki != a && osaki != b);
    assert_eq!(res.tracks_relinked, 1);
    assert!(res.phantom_deleted);

    // track_artist rebuilt to the three, in order, role `main`.
    assert_eq!(
        credits(&pool, t).await,
        vec![
            (a, "main".into(), 0),
            (b, "main".into(), 1),
            (osaki, "main".into(), 2),
        ]
    );
    // Primary repointed to the first fragment; phantom row gone.
    assert_eq!(primary_of(&pool, t).await, Some(a));
    assert!(!artist_exists(&pool, phantom).await);
}

#[tokio::test]
async fn deduplicates_repeated_fragments() {
    let pool = pool().await;
    let phantom = seed_artist(&pool, "A, B, A").await;
    let t = seed_track(&pool, 1, Some(phantom)).await;
    link(&pool, t, phantom, "main", 0).await;

    let res = split_artist_inner(&pool, phantom).await.unwrap();

    // "A, B, A" collapses to two distinct artists.
    let names: Vec<&str> = res.artists.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["A", "B"]);
    assert_eq!(credits(&pool, t).await.len(), 2);
}

#[tokio::test]
async fn keeps_the_phantom_when_still_referenced() {
    let pool = pool().await;
    let phantom = seed_artist(&pool, "A, B").await;
    let t = seed_track(&pool, 1, Some(phantom)).await;
    link(&pool, t, phantom, "main", 0).await;
    // An album still credits the phantom as its artist → the cleanup
    // guard must leave the row alone (deleting it would SET NULL the
    // album's artist).
    sqlx::query(
        "INSERT INTO album (id, title, canonical_title, artist_id) VALUES (1, 'Al', 'al', ?)",
    )
    .bind(phantom)
    .execute(&pool)
    .await
    .unwrap();

    let res = split_artist_inner(&pool, phantom).await.unwrap();

    assert!(!res.phantom_deleted);
    assert!(artist_exists(&pool, phantom).await);
    // ...but the track is still relinked away from the phantom.
    assert!(credits(&pool, t)
        .await
        .iter()
        .all(|(aid, _, _)| *aid != phantom));
}

#[tokio::test]
async fn preserves_cocredit_order_and_roles() {
    let pool = pool().await;
    let x = seed_artist(&pool, "X").await;
    let y = seed_artist(&pool, "Y").await;
    let phantom = seed_artist(&pool, "A, B").await;
    let t = seed_track(&pool, 1, Some(x)).await;
    // X(main,0), phantom(main,1), Y(feature,2).
    link(&pool, t, x, "main", 0).await;
    link(&pool, t, phantom, "main", 1).await;
    link(&pool, t, y, "feature", 2).await;

    let res = split_artist_inner(&pool, phantom).await.unwrap();
    let a = res.artists[0].id;
    let b = res.artists[1].id;

    // Phantom expanded in place; co-credits + roles kept, positions
    // renumbered contiguously.
    assert_eq!(
        credits(&pool, t).await,
        vec![
            (x, "main".into(), 0),
            (a, "main".into(), 1),
            (b, "main".into(), 2),
            (y, "feature".into(), 3),
        ]
    );
    // Primary was X (not the phantom) → untouched. Phantom had no
    // other references → deleted.
    assert_eq!(primary_of(&pool, t).await, Some(x));
    assert!(res.phantom_deleted);
}

#[tokio::test]
async fn inherits_the_phantoms_role_when_not_main() {
    let pool = pool().await;
    let x = seed_artist(&pool, "X").await;
    let phantom = seed_artist(&pool, "A, B").await;
    let t = seed_track(&pool, 1, Some(x)).await;
    // X is the main artist; the phantom is credited as a `feature`.
    link(&pool, t, x, "main", 0).await;
    link(&pool, t, phantom, "feature", 1).await;

    let res = split_artist_inner(&pool, phantom).await.unwrap();
    let a = res.artists[0].id;
    let b = res.artists[1].id;

    // Both fragments inherit the phantom's `feature` role, not `main`.
    assert_eq!(
        credits(&pool, t).await,
        vec![
            (x, "main".into(), 0),
            (a, "feature".into(), 1),
            (b, "feature".into(), 2),
        ]
    );
}

#[tokio::test]
async fn refuses_a_name_without_a_comma() {
    let pool = pool().await;
    let solo = seed_artist(&pool, "Solo").await;
    assert!(split_artist_inner(&pool, solo).await.is_err());
}
/// #851: the separators a CJK credit is written with split the same
/// way the ASCII comma does, and can be mixed in one name.
#[test]
fn cjk_commas_separate_names() {
    assert_eq!(split_fragments("庄心妍、祁隆"), vec!["庄心妍", "祁隆"]);
    assert_eq!(split_fragments("蔡依林，周杰伦"), vec!["蔡依林", "周杰伦"]);
    assert_eq!(split_fragments("A､B, C"), vec!["A", "B", "C"]);
    assert_eq!(split_fragments("Solo"), vec!["Solo"]);
}
