use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// The real migrator, `foreign_keys` on.
async fn pool() -> SqlitePool {
    let options = waveflow_core::repository::sqlite::collation::register(
        SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true),
    );
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
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0);
             INSERT INTO library_folder (id, library_id, path) VALUES (1, 1, '/m');
             INSERT INTO artist (id, name, canonical_name)
                  VALUES (1, 'Band', 'band'), (2, 'Other', 'other');",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn add_track(pool: &SqlitePool, id: i64, title: &str, artist: i64, duration_ms: i64) {
    sqlx::query(
        "INSERT INTO track (id, library_id, folder_id, title, primary_artist, duration_ms,
                                file_path, file_hash, file_size, file_modified, added_at,
                                is_available)
             VALUES (?, 1, 1, ?, ?, ?, ?, ?, 0, 0, 0, 1)",
    )
    .bind(id)
    .bind(title)
    .bind(artist)
    .bind(duration_ms)
    .bind(format!("/m/{id}.flac"))
    .bind(format!("hash{id}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_artist (track_id, artist_id, role, position) VALUES (?, ?, 'main', 0)",
    )
    .bind(id)
    .bind(artist)
    .execute(pool)
    .await
    .unwrap();
}

fn entry(title: &str, artist: &str, duration_ms: i64) -> ImportEntry {
    ImportEntry {
        title: title.into(),
        artist: Some(artist.into()),
        duration_ms: Some(duration_ms),
        ..ImportEntry::default()
    }
}

/// The playlist as the view would show it: `T<id>` for a track,
/// `P:<title>` for a placeholder, in display order.
async fn shown(pool: &SqlitePool, playlist_id: i64) -> Vec<String> {
    let mut rows: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "SELECT position, 1, 0, 'T' || track_id FROM playlist_track WHERE playlist_id = ?1
             UNION ALL
             SELECT position, 0, ordinal, 'P:' || title FROM playlist_placeholder WHERE playlist_id = ?1",
    )
    .bind(playlist_id)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.sort();
    rows.into_iter().map(|r| r.3).collect()
}

async fn import(pool: &SqlitePool, entries: Vec<ImportEntry>) -> (i64, ImportCounts) {
    let resolved = resolve(pool, entries).await.unwrap();
    create(pool, "Imported", ImportSource::Csv, &resolved)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_import_keeps_the_order_with_placeholders_between_tracks() {
    let pool = pool().await;
    add_track(&pool, 10, "Have", 1, 200_000).await;
    add_track(&pool, 11, "Also have", 1, 180_000).await;
    let (id, counts) = import(
        &pool,
        vec![
            entry("Missing first", "Band", 100_000),
            entry("Have", "Band", 201_000),
            entry("Missing a", "Band", 100_000),
            entry("Missing b", "Band", 100_000),
            entry("Also have", "Band", 180_000),
            entry("Have", "Band", 200_000),
            entry("Missing last", "Band", 100_000),
        ],
    )
    .await;
    assert_eq!(
        counts,
        ImportCounts {
            imported: 2,
            placeholders: 4,
            duplicates: 1
        }
    );
    assert_eq!(
        shown(&pool, id).await,
        [
            "P:Missing first",
            "T10",
            "P:Missing a",
            "P:Missing b",
            "T11",
            "P:Missing last"
        ]
    );
    // The counts are the tracks the user has, and only those.
    let (tracks, duration, missing): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), SUM(t.duration_ms),
                    (SELECT COUNT(*) FROM playlist_placeholder WHERE playlist_id = ?1)
               FROM playlist_track pt JOIN track t ON t.id = pt.track_id
              WHERE pt.playlist_id = ?1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((tracks, duration, missing), (2, 380_000, 4));
}

#[tokio::test]
async fn a_scan_that_brings_the_track_fills_its_placeholder_in_place() {
    let pool = pool().await;
    add_track(&pool, 10, "Have", 1, 200_000).await;
    let (id, _) = import(
        &pool,
        vec![
            entry("Later", "Band", 150_000),
            entry("Missing a", "Band", 100_000),
            entry("Have", "Band", 200_000),
        ],
    )
    .await;
    assert_eq!(shown(&pool, id).await, ["P:Later", "P:Missing a", "T10"]);

    add_track(&pool, 20, "Later", 1, 151_000).await;
    let changed = placeholders::fill_pending(&pool, 1).await.unwrap();
    assert_eq!(changed, [id]);
    assert_eq!(shown(&pool, id).await, ["T20", "P:Missing a", "T10"]);
    // Nothing left to fill: the second pass changes nothing.
    assert!(placeholders::fill_pending(&pool, 2)
        .await
        .unwrap()
        .is_empty());
    let positions: Vec<i64> = sqlx::query_scalar(
        "SELECT position FROM playlist_track WHERE playlist_id = ? ORDER BY position",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(positions, [0, 1]);
}

#[tokio::test]
async fn the_wrong_artist_does_not_fill_a_placeholder() {
    let pool = pool().await;
    let (id, _) = import(&pool, vec![entry("Song", "Band", 200_000)]).await;
    add_track(&pool, 30, "Song", 2, 200_000).await;
    assert!(placeholders::fill_pending(&pool, 1)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(shown(&pool, id).await, ["P:Song"]);
}

#[tokio::test]
async fn a_placeholder_for_a_track_already_held_is_dropped() {
    let pool = pool().await;
    add_track(&pool, 10, "Have", 1, 200_000).await;
    let (id, _) = import(
        &pool,
        vec![
            entry("Have", "Band", 200_000),
            entry("Have (Remastered)", "Band", 260_000),
        ],
    )
    .await;
    // Re-tagged to the length the placeholder expects: now it is a
    // match, but the playlist already holds the track.
    sqlx::query("UPDATE track SET duration_ms = 260000 WHERE id = 10")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(placeholders::fill_pending(&pool, 1).await.unwrap(), [id]);
    assert_eq!(shown(&pool, id).await, ["T10"]);
}

#[tokio::test]
async fn removing_and_moving_tracks_keeps_placeholders_beside_their_neighbours() {
    use waveflow_core::repository::sqlite::playlist::{remove_track_conn, reorder_track_conn};
    let pool = pool().await;
    for (id, title) in [(1, "A"), (2, "B"), (3, "C")] {
        add_track(&pool, id, title, 1, 100_000).await;
    }
    let (pl, _) = import(
        &pool,
        vec![
            entry("A", "Band", 100_000),
            entry("x", "Band", 1),
            entry("B", "Band", 100_000),
            entry("y", "Band", 1),
            entry("C", "Band", 100_000),
        ],
    )
    .await;
    assert_eq!(shown(&pool, pl).await, ["T1", "P:x", "T2", "P:y", "T3"]);

    // One connection in the pool: each write takes it and gives it
    // back before `shown` reads.
    let conn = || async { pool.acquire().await.unwrap() };
    // C to the top: it jumps over both placeholders, which stay
    // before the tracks they were before.
    reorder_track_conn(&mut *conn().await, pl, 3, 0, 1)
        .await
        .unwrap();
    assert_eq!(shown(&pool, pl).await, ["T3", "T1", "P:x", "T2", "P:y"]);
    // And back to the last place among the tracks. y had become the
    // tail, after B, and the tail stays after the last track.
    reorder_track_conn(&mut *conn().await, pl, 3, 2, 2)
        .await
        .unwrap();
    assert_eq!(shown(&pool, pl).await, ["T1", "P:x", "T2", "T3", "P:y"]);
    // B leaves: x now sits before the track that took its place.
    remove_track_conn(&mut *conn().await, pl, 2, 3)
        .await
        .unwrap();
    assert_eq!(shown(&pool, pl).await, ["T1", "P:x", "T3", "P:y"]);
}

#[tokio::test]
async fn gaps_that_meet_keep_each_one_s_order() {
    use waveflow_core::repository::sqlite::playlist::{remove_track_conn, reorder_track_conn};
    let pool = pool().await;
    for (id, title) in [(1, "A"), (2, "B"), (3, "C")] {
        add_track(&pool, id, title, 1, 100_000).await;
    }
    let missing = |t: &str| entry(t, "Band", 1);
    let (pl, _) = import(
        &pool,
        vec![
            missing("a1"),
            missing("a2"),
            entry("A", "Band", 100_000),
            missing("b1"),
            missing("b2"),
            entry("B", "Band", 100_000),
            missing("c1"),
            missing("c2"),
            entry("C", "Band", 100_000),
        ],
    )
    .await;
    let conn = || async { pool.acquire().await.unwrap() };
    // A leaves: the gap before it and the one before B become one,
    // in their order.
    remove_track_conn(&mut *conn().await, pl, 1, 1)
        .await
        .unwrap();
    assert_eq!(
        shown(&pool, pl).await,
        ["P:a1", "P:a2", "P:b1", "P:b2", "T2", "P:c1", "P:c2", "T3"]
    );
    // B down past C: the gaps either side of B's old place meet.
    reorder_track_conn(&mut *conn().await, pl, 2, 1, 2)
        .await
        .unwrap();
    assert_eq!(
        shown(&pool, pl).await,
        ["P:a1", "P:a2", "P:b1", "P:b2", "P:c1", "P:c2", "T3", "T2"]
    );
    // And back up: the gap B leaves meets the one before the track
    // after it, placed ahead of it.
    reorder_track_conn(&mut *conn().await, pl, 2, 0, 3)
        .await
        .unwrap();
    assert_eq!(
        shown(&pool, pl).await,
        ["T2", "P:a1", "P:a2", "P:b1", "P:b2", "P:c1", "P:c2", "T3"]
    );
}

#[tokio::test]
async fn an_appended_track_lands_after_the_tail_placeholders() {
    use waveflow_core::repository::sqlite::playlist::append_track_conn;
    let pool = pool().await;
    add_track(&pool, 1, "A", 1, 100_000).await;
    add_track(&pool, 2, "New", 2, 100_000).await;
    let (pl, _) = import(
        &pool,
        vec![entry("A", "Band", 100_000), entry("z", "Band", 1)],
    )
    .await;
    append_track_conn(&mut pool.acquire().await.unwrap(), pl, 2, 1)
        .await
        .unwrap();
    assert_eq!(shown(&pool, pl).await, ["T1", "P:z", "T2"]);
}

#[tokio::test]
async fn an_isrc_from_the_tags_matches_whatever_the_title() {
    let pool = pool().await;
    add_track(&pool, 5, "Titre traduit", 2, 222_000).await;
    sqlx::query(
        "INSERT INTO track_tag (track_id, key, value) VALUES (5, 'ISRC', 'GB-AYE-06-01690')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut wanted = entry("Original title", "Band", 222_000);
    wanted.isrc = Some("GBAYE0601690".into());
    let (pl, counts) = import(&pool, vec![wanted]).await;
    assert_eq!(counts.imported, 1);
    assert_eq!(shown(&pool, pl).await, ["T5"]);
}

#[test]
fn an_m3u_entry_without_extinf_is_named_after_its_file() {
    let parsed = m3u_entries(
        "#EXTM3U\n#EXTINF:120,A - B\nsub/one.flac\n/abs/two.mp3\n",
        std::path::Path::new("/lists"),
    );
    assert_eq!(parsed.entries[0].title, "B");
    assert_eq!(parsed.entries[0].artist.as_deref(), Some("A"));
    assert!(parsed.entries[0]
        .locator
        .as_deref()
        .is_some_and(|l| l.ends_with("one.flac") && l.contains("lists")));
    assert_eq!(parsed.entries[1].title, "two");
}
