use super::*;

/// The repo's profile migrations, with `foreign_keys` on as on every
/// profile pool.
async fn pool(dir: &std::path::Path) -> SqlitePool {
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;
    let opts =
        SqliteConnectOptions::from_str(&format!("sqlite://{}", dir.join("profile.db").display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true);
    let pool = SqlitePool::connect_with(opts).await.unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

/// Track 1 credited to Giga then TeddyLoid (the credit rows inserted in
/// the opposite order, so only `position` can put Giga first), track 2
/// with a single artist, track 3 with none.
async fn library(pool: &SqlitePool) {
    sqlx::raw_sql(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
         INSERT INTO artist (id, name, canonical_name) VALUES
             (1, 'TeddyLoid', 'teddyloid'), (2, 'Giga', 'giga'), (3, 'Solo', 'solo');
         INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                            file_modified, title, primary_artist, duration_ms, added_at)
         VALUES (1, 1, '/l/1.flac', 'h1', 1, 0, 'Two artists', 2, 200000, 0),
                (2, 1, '/l/2.flac', 'h2', 1, 0, 'One artist', 3, 200000, 0),
                (3, 1, '/l/3.flac', 'h3', 1, 0, 'No artist', NULL, 200000, 0);
         INSERT INTO track_artist (track_id, artist_id, role, position) VALUES
             (1, 1, 'main', 1), (1, 2, 'main', 0), (2, 3, 'main', 0);",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// The reported case (#1075): the queue hands the services the primary
/// artist, not the joined credit, and keeps the whole credit apart for
/// ListenBrainz.
#[tokio::test]
async fn a_queued_scrobble_names_the_primary_artist() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    for provider in [LASTFM, LISTENBRAINZ] {
        enqueue(&pool, provider, 1, 1_700_000_000_000, 200_000)
            .await
            .unwrap();
        let due = fetch_due(&pool, provider).await.unwrap();
        assert_eq!(due.pending.len(), 1);
        assert_eq!(due.pending[0].artist_name, "Giga");
        assert_eq!(due.pending[0].artist_names, ["Giga", "TeddyLoid"]);
        delete_item(&pool, due.pending[0].id).await.unwrap();
    }
}

#[tokio::test]
async fn a_single_artist_track_scrobbles_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    enqueue(&pool, LASTFM, 2, 1_700_000_000_000, 200_000)
        .await
        .unwrap();
    let due = fetch_due(&pool, LASTFM).await.unwrap();
    assert_eq!(due.pending[0].artist_name, "Solo");
    assert_eq!(due.pending[0].artist_names, ["Solo"]);
}

/// A track with no credited artist cannot be scrobbled: its row is
/// flagged for deletion instead of being re-read every tick.
#[tokio::test]
async fn a_track_without_artist_is_poison() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    enqueue(&pool, LASTFM, 3, 1_700_000_000_000, 200_000)
        .await
        .unwrap();
    let due = fetch_due(&pool, LASTFM).await.unwrap();
    assert!(due.pending.is_empty());
    assert_eq!(due.poison_ids.len(), 1);
}

/// The now-playing announcement reads the same credit, same order.
#[tokio::test]
async fn the_announcement_reads_the_credit_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    assert_eq!(artists_of_track(&pool, 1).await, ["Giga", "TeddyLoid"]);
    assert_eq!(artists_of_track(&pool, 2).await, ["Solo"]);
    assert!(artists_of_track(&pool, 3).await.is_empty());
}

#[test]
fn the_joined_column_splits_on_the_separator_only() {
    // A band name with a comma stays one artist.
    let joined = "Tyler, The Creator\u{1f} Kali Uchis \u{1f}\u{1f}";
    assert_eq!(
        split_artist_names(Some(joined)),
        ["Tyler, The Creator", "Kali Uchis"]
    );
    assert!(split_artist_names(None).is_empty());
}
