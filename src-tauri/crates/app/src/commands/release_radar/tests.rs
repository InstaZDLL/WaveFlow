//! The radar's tables and the playlist built from them, against the real
//! profile schema with `foreign_keys` on: the migration has to accept
//! what the store writes, and an artist leaving the library has to take
//! its radar rows with it.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

use waveflow_core::release_radar::{store, ReleaseGroup};
use waveflow_core::smart_playlists::release_radar::regenerate_release_radar;

use super::PathsContext;

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
    pool
}

async fn run(pool: &SqlitePool, statements: &[String]) {
    for statement in statements {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement.clone()))
            .execute(pool)
            .await
            .unwrap();
    }
}

/// Artists: 1 Nova (3 tracks, album "Record (Deluxe Edition)"), 2 Quiet
/// (1 track, liked and played eight times), 3 Various Artists (5 tracks).
async fn seed(pool: &SqlitePool) {
    let mut s = vec![
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                              hlc_wall, hlc_logical)
         VALUES (1, 'L', 1, 1, 0, 0, 0, 0)"
            .to_string(),
        "INSERT INTO artist (id, name, canonical_name) VALUES
             (1, 'Nova', 'nova'), (2, 'Quiet', 'quiet'),
             (3, 'Various Artists', 'various artists')"
            .to_string(),
        "INSERT INTO album (id, title, canonical_title, artist_id) VALUES
             (10, 'Record (Deluxe Edition)', 'record deluxe edition', 1),
             (11, 'Elsewhere', 'elsewhere', 2),
             (12, 'Hits', 'hits', 3)"
            .to_string(),
    ];
    let mut track = |id: i64, artist: i64, album: i64, title: &str, number: i64| {
        s.push(format!(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, track_number, duration_ms, added_at,
                                is_available, hlc_wall, hlc_logical, rating_hlc_wall,
                                rating_hlc_logical, primary_artist)
             VALUES ({id}, 1, '/m/{id}.flac', 'h{id}', 1, 0, '{title}', {album}, {number},
                     1000, 0, 1, 0, 0, 0, 0, {artist})"
        ));
        s.push(format!(
            "INSERT INTO track_artist (track_id, artist_id, position) VALUES ({id}, {artist}, 0)"
        ));
    };
    track(102, 1, 10, "Second", 2);
    track(101, 1, 10, "First", 1);
    track(103, 1, 10, "Third", 3);
    track(201, 2, 11, "Hush", 1);
    for n in 0..5 {
        track(301 + n, 3, 12, "Hit", n + 1);
    }
    let now = chrono::Utc::now().timestamp_millis();
    // Quiet: one liked track (3) and eight plays (4) outweigh Nova's 3.
    s.push(format!(
        "INSERT INTO liked_track (track_id, liked_at) VALUES (201, {now})"
    ));
    for _ in 0..8 {
        s.push(format!(
            "INSERT INTO play_event (track_id, played_at, listened_ms, completed, skipped)
             VALUES (201, {now}, 60000, 1, 0)"
        ));
    }
    run(pool, &s).await;
}

fn group(id: &str, title: &str, date: &str, kind: &str) -> ReleaseGroup {
    ReleaseGroup {
        id: id.to_string(),
        title: title.to_string(),
        artist_credit: "Nova".to_string(),
        first_release_date: date.to_string(),
        primary_type: Some(kind.to_string()),
        secondary_types: vec![],
        credit_mbid: None,
    }
}

fn ctx(dir: &std::path::Path) -> PathsContext {
    PathsContext {
        metadata_artwork_dir: dir.join("metadata"),
        app_db_path: dir.join("app.db"),
        profile_artwork_dir: dir.join("artwork"),
    }
}

fn recent() -> String {
    (chrono::Utc::now() - chrono::Duration::days(10))
        .format("%Y-%m-%d")
        .to_string()
}

#[tokio::test]
async fn artists_are_weighed_by_tracks_plays_and_likes_and_various_artists_is_not_one() {
    let pool = pool().await;
    seed(&pool).await;
    let now = chrono::Utc::now().timestamp_millis();
    let followed = store::followed_artists(&pool, now, 100).await.unwrap();
    let names: Vec<_> = followed.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["Quiet", "Nova"]);
    // 1 track + 8 plays / 2 + 3 × 1 liked.
    assert_eq!(followed[0].weight, 8);
    assert_eq!(followed[1].weight, 3);
    assert!(followed
        .iter()
        .all(|a| a.checked_at.is_none() && a.mbid.is_none()));
}

#[tokio::test]
async fn an_answer_checks_the_artist_and_keeps_an_id_it_learnt() {
    let pool = pool().await;
    seed(&pool).await;
    let releases = [group("rg-1", "Record", &recent(), "Album")];
    store::record_artist(&pool, 1, Some("mb-nova"), &releases, 1_000)
        .await
        .unwrap();
    // A later answer that teaches nothing keeps the id.
    store::record_artist(&pool, 1, None, &releases, 2_000)
        .await
        .unwrap();
    let nova = store::followed_artists(&pool, 0, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.artist_id == 1)
        .unwrap();
    assert_eq!(nova.mbid.as_deref(), Some("mb-nova"));
    assert_eq!(nova.checked_at, Some(2_000));
    let discovered: i64 =
        sqlx::query_scalar("SELECT discovered_at FROM release_radar_release WHERE id = 'rg-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(discovered, 1_000, "an upsert keeps when it was first seen");

    // A full rescan forgets the learnt id too: the way back from a wrong one.
    store::reset_checks(&pool).await.unwrap();
    let nova = store::followed_artists(&pool, 0, 100).await.unwrap();
    assert!(nova
        .iter()
        .all(|a| a.checked_at.is_none() && a.mbid.is_none()));
}

async fn followed_mbid(pool: &SqlitePool, artist_id: i64) -> Option<String> {
    store::followed_artists(pool, 0, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.artist_id == artist_id)
        .unwrap()
        .mbid
}

#[tokio::test]
async fn an_artist_id_accepted_from_the_enrichment_engine_comes_first() {
    let pool = pool().await;
    seed(&pool).await;
    store::record_artist(&pool, 1, Some("mb-learnt"), &[], 1_000)
        .await
        .unwrap();

    // A proposal still waiting on review is not the user's word yet.
    sqlx::query(
        "INSERT INTO musicbrainz_artist (artist_id, status, artist_mbid, matched_at)
         VALUES (1, 'review', 'mb-accepted', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(followed_mbid(&pool, 1).await.as_deref(), Some("mb-learnt"));

    sqlx::query("UPDATE musicbrainz_artist SET status = 'applied' WHERE artist_id = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        followed_mbid(&pool, 1).await.as_deref(),
        Some("mb-accepted")
    );
}

#[tokio::test]
async fn the_library_confirms_an_id_only_through_a_release_it_holds() {
    let pool = pool().await;
    seed(&pool).await;
    let tracks = store::local_tracks_of(&pool, 1).await.unwrap();
    assert_eq!(tracks.len(), 3, "Nova's three tracks, once each");
    let mut record = group("rec", "Record", &recent(), "Album");
    record.credit_mbid = Some("mb-nova".into());
    let mut other = group("new", "Unheard", &recent(), "Album");
    other.credit_mbid = Some("mb-homonym".into());
    let refs: Vec<_> = [&record, &other]
        .iter()
        .map(|r| waveflow_core::release_radar::ReleaseRef {
            id: r.id.clone(),
            artist_id: 1,
            title: r.title.clone(),
            primary_type: r.primary_type.clone(),
        })
        .collect();
    let releases = vec![record.clone(), other.clone()];
    let matches = waveflow_core::release_radar::match_releases(&refs, &tracks);
    assert_eq!(
        waveflow_core::release_radar::confirmed_mbid(&releases, &matches).as_deref(),
        Some("mb-nova")
    );
    // Without the record in the library, the homonym's release proves nothing.
    let only_other = vec![other];
    let matches = waveflow_core::release_radar::match_releases(&refs[1..], &tracks);
    assert_eq!(
        waveflow_core::release_radar::confirmed_mbid(&only_other, &matches),
        None
    );
}

#[tokio::test]
async fn releases_older_than_the_window_are_pruned_partial_dates_included() {
    let pool = pool().await;
    seed(&pool).await;
    let releases = [
        group("y", "Year only", "2026", "Album"),
        group("m", "Month only", "2026-03", "Album"),
        group("d", "Day", "2026-03-15", "Album"),
    ];
    store::record_artist(&pool, 1, None, &releases, 0)
        .await
        .unwrap();
    assert_eq!(
        store::prune_older_than(&pool, "2026-03-01").await.unwrap(),
        1
    );
    let left: Vec<String> = store::recent_releases(&pool, "2000-01-01")
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(left, ["d", "m"]);
}

#[tokio::test]
async fn the_playlist_holds_the_playable_releases_only_and_goes_when_none_is() {
    let pool = pool().await;
    seed(&pool).await;
    let dir = tempfile::tempdir().unwrap();
    // Only discovered: no playlist.
    store::record_artist(
        &pool,
        1,
        None,
        &[group("new", "Unheard", &recent(), "Album")],
        0,
    )
    .await
    .unwrap();
    let none = regenerate_release_radar(&pool, &ctx(dir.path()), 1, "d")
        .await
        .unwrap();
    assert_eq!(none, None);

    // The library holds "Record": its tracks, in album order.
    store::record_artist(
        &pool,
        1,
        None,
        &[group("rec", "Record", &recent(), "Album")],
        0,
    )
    .await
    .unwrap();
    let id = regenerate_release_radar(&pool, &ctx(dir.path()), 1, "New from your artists")
        .await
        .unwrap()
        .expect("a playable release makes a playlist");
    let tracks: Vec<i64> = sqlx::query_scalar(
        "SELECT track_id FROM playlist_track WHERE playlist_id = ? ORDER BY position",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(tracks, [101, 102, 103]);
    let (name, rules): (String, String) =
        sqlx::query_as("SELECT name, smart_rules FROM playlist WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(name, "Release Radar");
    assert_eq!(rules, r#"{"kind":"release_radar"}"#);

    // The same row is rewritten, not stacked.
    let again = regenerate_release_radar(&pool, &ctx(dir.path()), 1, "d")
        .await
        .unwrap();
    assert_eq!(again, Some(id));

    // The album leaves the library: the playlist goes with it.
    sqlx::query("UPDATE track SET is_available = 0 WHERE album_id = 10")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        regenerate_release_radar(&pool, &ctx(dir.path()), 1, "d")
            .await
            .unwrap(),
        None
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist WHERE is_smart = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn a_cover_is_asked_for_once_then_again_after_its_delay() {
    let pool = pool().await;
    seed(&pool).await;
    store::record_artist(
        &pool,
        1,
        None,
        &[group("rg", "Record", &recent(), "Album")],
        0,
    )
    .await
    .unwrap();
    assert_eq!(store::covers_due(&pool, 1_000, 500).await.unwrap(), ["rg"]);
    store::record_cover(&pool, "rg", None, 1_000).await.unwrap();
    assert!(store::covers_due(&pool, 1_200, 500)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(store::covers_due(&pool, 1_600, 500).await.unwrap(), ["rg"]);
    store::record_cover(&pool, "rg", Some("hash"), 1_600)
        .await
        .unwrap();
    // A cover the cache has is never asked for again, and a later miss
    // does not erase it.
    store::record_cover(&pool, "rg", None, 9_000).await.unwrap();
    assert!(store::covers_due(&pool, 99_000, 500)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn an_artist_leaving_the_library_takes_its_radar_rows_with_it() {
    let pool = pool().await;
    seed(&pool).await;
    store::record_artist(
        &pool,
        2,
        Some("mb-quiet"),
        &[group("rg", "Hush", &recent(), "Single")],
        0,
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM track WHERE id = 201")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM artist WHERE id = 2")
        .execute(&pool)
        .await
        .unwrap();
    let left: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM release_radar_artist),
                (SELECT COUNT(*) FROM release_radar_release)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, (0, 0));
}

#[tokio::test]
async fn a_single_matches_the_track_it_names() {
    let pool = pool().await;
    seed(&pool).await;
    store::record_artist(
        &pool,
        2,
        None,
        &[group("s", "Hush - Single", &recent(), "Single")],
        0,
    )
    .await
    .unwrap();
    let refs: Vec<_> = store::recent_releases(&pool, "2000-01-01")
        .await
        .unwrap()
        .iter()
        .map(|r| r.as_ref())
        .collect();
    let tracks = store::local_tracks(&pool).await.unwrap();
    let found = waveflow_core::release_radar::match_releases(&refs, &tracks);
    assert_eq!(found["s"].track_ids, [201]);
}

#[test]
fn an_artist_is_due_when_never_asked_or_asked_long_enough_ago() {
    use waveflow_core::release_radar::REFRESH_AFTER_MS;
    let now = 10 * REFRESH_AFTER_MS;
    assert!(super::is_due(None, now));
    assert!(super::is_due(Some(now - REFRESH_AFTER_MS - 1), now));
    assert!(!super::is_due(Some(now - REFRESH_AFTER_MS), now));
    assert!(!super::is_due(Some(now), now));
}
