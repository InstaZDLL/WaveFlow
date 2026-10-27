use super::*;
use sqlx::sqlite::SqlitePoolOptions;

/// Serialises the tests that touch [`MIRROR_PHASE`].
///
/// The phase is process-global while test threads are not: one test
/// holding the slot makes another's `clear` fail, which reads as a bug in
/// `clear` rather than as two tests colliding. A tokio mutex rather than a
/// std one because it is held across `.await`.
static PHASE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `foreign_keys` ON, like the real profile database: a fixture that
/// leaves it off would let a broken delete pass. See the projection
/// tests for the same migration list.
async fn pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(":memory:")
        .await
        .unwrap();
    sqlx::raw_sql("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    // The real migrator, like every other remote fixture: a hand-listed
    // subset breaks whenever a migration touches a table the list
    // happened to omit. The upgrade test below lists them on purpose,
    // because it needs the schema as it was *before* one of them.
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

fn album(id: &str, song_count: i64) -> AlbumListItem {
    AlbumListItem {
        id: id.into(),
        library_id: Some("lib-1".into()),
        title: format!("Album {id}"),
        artist: Some("Artist".into()),
        artist_id: Some("art-1".into()),
        artwork_hash: None,
        year: Some(1999),
        is_compilation: false,
        sort_name: None,
        song_count,
        duration_ms: 1_000,
        created_at: Some(10),
    }
}

/// `upsert_album` writes through the page transaction in production; the
/// tests upsert one album at a time.
async fn upsert(pool: &SqlitePool, listed: &AlbumListItem) {
    let mut tx = pool.begin().await.unwrap();
    upsert_album(&mut tx, listed).await.unwrap();
    tx.commit().await.unwrap();
}

async fn mirrored_track(pool: &SqlitePool, id: &str) {
    sqlx::query(
        "INSERT INTO remote_track (remote_id, title, artist_id, duration_ms, cached_at, in_catalogue)
             VALUES (?, ?, 'art-1', 0, 1, 1)",
    )
    .bind(id)
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn an_album_is_fresh_only_once_walked_with_the_same_count() {
    let pool = pool().await;
    let listed = album("al-1", 12);

    // Never seen: not fresh.
    assert!(!album_is_fresh(&pool, &listed).await.unwrap());

    // Listed but not yet walked: still not fresh — `mirrored_at` is NULL,
    // which is exactly the state an interrupted walk leaves behind.
    upsert(&pool, &listed).await;
    assert!(!album_is_fresh(&pool, &listed).await.unwrap());

    sqlx::query("UPDATE remote_album SET mirrored_at = 1 WHERE remote_id = 'al-1'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(album_is_fresh(&pool, &listed).await.unwrap());

    // The server gained a track: the count no longer matches, so it is
    // fetched again.
    assert!(!album_is_fresh(&pool, &album("al-1", 13)).await.unwrap());
}

/// The listing is the source of truth for an album's own fields; a second
/// pass must overwrite them rather than accumulate stale ones.
#[tokio::test]
async fn re_listing_an_album_updates_it_in_place() {
    let pool = pool().await;
    upsert(&pool, &album("al-1", 3)).await;
    let mut renamed = album("al-1", 4);
    renamed.title = "Renamed".into();
    renamed.is_compilation = true;
    upsert(&pool, &renamed).await;

    let (count, title, compilation): (i64, String, i64) = sqlx::query_as(
        "SELECT song_count, title, is_compilation FROM remote_album WHERE remote_id = 'al-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((count, title.as_str(), compilation), (4, "Renamed", 1));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

/// The whole point of `in_catalogue`: dropping the mirror must not take a
/// playlist's rows with it, or the playlist loses its titles.
#[tokio::test]
async fn dropping_the_mirror_spares_rows_the_user_data_needs() {
    // `clear` claims the walk slot, so this shares it with the test below.
    let _lock = PHASE_LOCK.lock().await;
    let pool = pool().await;
    mirrored_track(&pool, "t-keep").await;
    mirrored_track(&pool, "t-drop").await;
    sqlx::query("INSERT INTO remote_playlist (remote_id, name, updated_at) VALUES ('p1','P',1)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO remote_playlist_track (playlist_remote_id, position, track_remote_id)
             VALUES ('p1', 0, 't-keep')",
    )
    .execute(&pool)
    .await
    .unwrap();

    upsert(&pool, &album("al-1", 1)).await;
    upsert_one_artist(&pool, &artist("ar-1", "Someone", None)).await;

    clear(&pool).await.unwrap();

    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT remote_id, in_catalogue FROM remote_track")
            .fetch_all(&pool)
            .await
            .unwrap();
    // Kept, but no longer part of the catalogue.
    assert_eq!(rows, vec![("t-keep".to_string(), 0)]);

    // Nothing the catalogue owns outright survives the purge. Spelled out
    // rather than looped: sqlx refuses a runtime-formatted query.
    let albums: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_album")
        .fetch_one(&pool)
        .await
        .unwrap();
    let artists: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((albums, artists), (0, 0));
}

/// Same rule on the incremental path: a track the server dropped stops
/// counting as catalogue, and only disappears if nothing points at it.
#[tokio::test]
async fn a_vanished_track_is_unflagged_before_it_is_deleted() {
    let pool = pool().await;
    mirrored_track(&pool, "t-gone").await;
    mirrored_track(&pool, "t-liked").await;
    sqlx::query(
        "INSERT INTO remote_favorite (entity_type, entity_id, starred_at)
             VALUES ('track', 't-liked', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Both left the catalogue, but only the unreferenced one was deleted —
    // `removed` counts deletions, not visits.
    let removed = drop_vanished(&pool, &HashSet::new()).await.unwrap();
    assert_eq!(removed, 1);

    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT remote_id, in_catalogue FROM remote_track")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![("t-liked".to_string(), 0)]);
}

#[tokio::test]
async fn a_still_listed_track_survives_the_sweep() {
    let pool = pool().await;
    mirrored_track(&pool, "t-1").await;
    let seen: HashSet<String> = ["t-1".to_string()].into_iter().collect();

    assert_eq!(drop_vanished(&pool, &seen).await.unwrap(), 0);
    let flag: i64 =
        sqlx::query_scalar("SELECT in_catalogue FROM remote_track WHERE remote_id = 't-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(flag, 1);
}

/// A date on a half-swept mirror would read as "the catalogue is up to
/// date", which is the one thing it is not.
#[tokio::test]
async fn the_mirror_reports_no_date_until_every_library_is_swept() {
    let pool = pool().await;
    store_libraries(
        &pool,
        &[
            LibraryAccessDto {
                id: "lib-1".into(),
                name: "One".into(),
            },
            LibraryAccessDto {
                id: "lib-2".into(),
                name: "Two".into(),
            },
        ],
    )
    .await
    .unwrap();
    mirrored_track(&pool, "t-1").await;
    upsert(&pool, &album("al-1", 1)).await;

    let held = stats(&pool).await.unwrap();
    assert_eq!((held.libraries, held.tracks, held.albums), (2, 1, 1));
    // Listed but never walked.
    assert_eq!(held.albums_mirrored, 0);
    assert_eq!(held.mirrored_at, None);

    sqlx::query("UPDATE remote_library SET mirrored_at = 500 WHERE remote_id = 'lib-1'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(stats(&pool).await.unwrap().mirrored_at, None);

    sqlx::query("UPDATE remote_library SET mirrored_at = 900 WHERE remote_id = 'lib-2'")
        .execute(&pool)
        .await
        .unwrap();
    // The oldest sweep, not the newest: that is the age of the mirror.
    assert_eq!(stats(&pool).await.unwrap().mirrored_at, Some(500));
}

#[tokio::test]
async fn re_listing_a_library_keeps_its_sweep_date() {
    let pool = pool().await;
    let libraries = [LibraryAccessDto {
        id: "lib-1".into(),
        name: "One".into(),
    }];
    store_libraries(&pool, &libraries).await.unwrap();
    sqlx::query("UPDATE remote_library SET mirrored_at = 42")
        .execute(&pool)
        .await
        .unwrap();

    // A rename must not look like a library that was never swept.
    store_libraries(
        &pool,
        &[LibraryAccessDto {
            id: "lib-1".into(),
            name: "Renamed".into(),
        }],
    )
    .await
    .unwrap();

    let (name, mirrored): (String, Option<i64>) =
        sqlx::query_as("SELECT name, mirrored_at FROM remote_library")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((name.as_str(), mirrored), ("Renamed", Some(42)));
}

/// The unified listing sorts both halves against each other, so the
/// remote keys have to go through the same normaliser the local
/// `canonical_*` columns did — SQLite cannot fold a diacritic, and a list
/// that sorts "Björk" apart from "bjork" splits one artist in two.
#[tokio::test]
async fn the_sort_keys_are_normalised_like_the_local_ones() {
    let pool = pool().await;
    let mut listed = album("al-1", 1);
    listed.title = "Vespertine".into();
    listed.artist = Some("Björk".into());
    upsert(&pool, &listed).await;

    let (sort_title, sort_artist): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT sort_title, sort_artist FROM remote_album WHERE remote_id = 'al-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(sort_title.as_deref(), Some("vespertine"));
    assert_eq!(sort_artist.as_deref(), Some("bjork"));
}

/// The server's `sort_name` is a tagged display form, not a comparison
/// key, so it is normalised rather than stored as-is — and it wins over
/// the title when present, which is the whole point of a sort tag.
#[tokio::test]
async fn a_tagged_sort_name_is_the_input_to_the_key_not_the_key() {
    let pool = pool().await;
    let mut listed = album("al-2", 1);
    listed.title = "The Beatles".into();
    listed.sort_name = Some("Beatles, The".into());
    upsert(&pool, &listed).await;

    let sort_title: Option<String> =
        sqlx::query_scalar("SELECT sort_title FROM remote_album WHERE remote_id = 'al-2'")
            .fetch_one(&pool)
            .await
            .unwrap();
    // Punctuation dropped, lowercased — the form the local half stores.
    assert_eq!(sort_title.as_deref(), Some("beatles the"));
}

fn artist(id: &str, name: &str, sort_name: Option<&str>) -> ArtistListItem {
    ArtistListItem {
        id: id.into(),
        library_id: Some("lib-1".into()),
        name: name.into(),
        artwork_hash: Some("aa11".into()),
        sort_name: sort_name.map(Into::into),
    }
}

async fn upsert_one_artist(pool: &SqlitePool, listed: &ArtistListItem) {
    let mut tx = pool.begin().await.unwrap();
    upsert_artist(&mut tx, listed).await.unwrap();
    tx.commit().await.unwrap();
}

/// The artist grid sorts against the local half, whose key went through
/// the normaliser. Anything less puts "Björk" and "bjork" in two places.
#[tokio::test]
async fn an_artists_sort_key_is_normalised_like_the_local_one() {
    let pool = pool().await;
    upsert_one_artist(&pool, &artist("ar-1", "Björk", None)).await;
    upsert_one_artist(&pool, &artist("ar-2", "The Beatles", Some("Beatles, The"))).await;

    let keys: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT remote_id, sort_key FROM remote_artist ORDER BY remote_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(keys[0].1.as_deref(), Some("bjork"));
    // The tagged sort form is the input to the key, not the key.
    assert_eq!(keys[1].1.as_deref(), Some("beatles the"));
}

/// Re-listing must overwrite in place — a renamed artist is the same
/// artist, and a second row would double them in the grid.
#[tokio::test]
async fn re_listing_an_artist_updates_it_in_place() {
    let pool = pool().await;
    upsert_one_artist(&pool, &artist("ar-1", "Bjork", None)).await;
    upsert_one_artist(&pool, &artist("ar-1", "Björk", None)).await;

    let rows: Vec<(String, String)> = sqlx::query_as("SELECT remote_id, name FROM remote_artist")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows, vec![("ar-1".to_string(), "Björk".to_string())]);
}

/// A library the account lost access to must not keep its sweep date: a
/// mirror that can no longer see it would still read as complete.
#[tokio::test]
async fn a_library_that_disappears_is_dropped() {
    let pool = pool().await;
    store_libraries(
        &pool,
        &[
            LibraryAccessDto {
                id: "lib-1".into(),
                name: "One".into(),
            },
            LibraryAccessDto {
                id: "lib-2".into(),
                name: "Two".into(),
            },
        ],
    )
    .await
    .unwrap();

    store_libraries(
        &pool,
        &[LibraryAccessDto {
            id: "lib-1".into(),
            name: "One".into(),
        }],
    )
    .await
    .unwrap();

    let rows: Vec<String> = sqlx::query_scalar("SELECT remote_id FROM remote_library")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows, vec!["lib-1".to_string()]);
}

/// A server with no notion of unavailability omits the flag. Reading that
/// as "unavailable" would sweep the whole catalogue away on the first run.
#[test]
fn a_track_record_without_the_flag_counts_as_available() {
    let record: TrackRecordDto = serde_json::from_str(r#"{"id":"t-1"}"#).unwrap();
    assert!(record.available);

    let hidden: TrackRecordDto = serde_json::from_str(r#"{"id":"t-2","available":false}"#).unwrap();
    assert!(!hidden.available);
}

/// The listing carries far more than the mirror keeps; unknown fields must
/// not break the walk, and the ones we do read must land.
#[test]
fn an_album_listing_parses_past_the_fields_we_ignore() {
    let listed: AlbumListItem = serde_json::from_str(
        r#"{"id":"al-1","library_id":"lib-1","title":"Kind of Blue",
                "artist":"Miles Davis","artist_id":"art-1","artwork_hash":"abc",
                "year":1959,"is_compilation":false,"sort_name":"Kind of Blue",
                "song_count":5,"duration_ms":2643000,"created_at":17,
                "genres":["Jazz"],"release_types":["album"],"play_count":3}"#,
    )
    .unwrap();
    assert_eq!(listed.song_count, 5);
    assert_eq!(listed.year, Some(1959));
    assert_eq!(listed.artist_id.as_deref(), Some("art-1"));
}

/// The walk slot is process-global, so this exercises the whole machine in
/// one test rather than several: two tests sharing it would race each
/// other's assertions, not the code under test.
#[tokio::test]
async fn the_walk_slot_is_exclusive() {
    let _lock = PHASE_LOCK.lock().await;

    // Idle: nothing to cancel, and clearing is allowed.
    assert!(!request_cancel());
    assert!(!cancelled());
    let pool = pool().await;
    clear(&pool).await.unwrap();

    {
        // A walk owns the slot: clearing is refused rather than
        // interleaved. `PhaseGuard` is the production guard, and it is what
        // hands the slot back here — including when an assert below
        // unwinds, which would otherwise leave the phase RUNNING and fail
        // every later `clear` in the binary.
        MIRROR_PHASE.store(PHASE_RUNNING, Ordering::SeqCst);
        let _guard = PhaseGuard;
        assert!(clear(&pool).await.is_err());
        assert!(request_cancel());
        assert!(cancelled());
    }

    // The guard reset the phase on the way out, so the next clear goes
    // through — which is also what makes a failed walk not wedge the
    // feature for the rest of the session.
    assert_eq!(MIRROR_PHASE.load(Ordering::SeqCst), PHASE_IDLE);
    clear(&pool).await.unwrap();
}

/// The upgrade path for the track sort keys: an album already walked, whose
/// count has not changed, would otherwise never be walked again — so its
/// tracks would keep their empty keys and keep sorting on the wrong
/// expression forever. The migration clears the stamps to force one walk.
#[tokio::test]
async fn the_sort_key_migration_makes_every_walked_album_stale() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(":memory:")
        .await
        .unwrap();
    // Everything up to, but not including, the migration under test.
    for migration in [
        include_str!(
            "../../../../../migrations/profile/20260810120000_remote_source_projection.sql"
        ),
        include_str!("../../../../../migrations/profile/20260810140000_remote_track_cache.sql"),
        include_str!("../../../../../migrations/profile/20260813090000_remote_track_full_hash.sql"),
        include_str!("../../../../../migrations/profile/20260816120000_remote_track_artist_id.sql"),
        include_str!(
            "../../../../../migrations/profile/20260824210000_remote_catalogue_mirror.sql"
        ),
        include_str!("../../../../../migrations/profile/20260826090000_remote_album_sort_keys.sql"),
    ] {
        sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    }

    let listed = album("al-1", 5);
    upsert(&pool, &listed).await;
    sqlx::query("UPDATE remote_album SET mirrored_at = 1 WHERE remote_id = 'al-1'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        album_is_fresh(&pool, &listed).await.unwrap(),
        "the album is walked and unchanged before the upgrade"
    );

    sqlx::raw_sql(include_str!(
        "../../../../../migrations/profile/20260826180000_remote_track_sort_keys.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();

    assert!(
        !album_is_fresh(&pool, &listed).await.unwrap(),
        "the upgrade must send it back through the walk"
    );
}

/// The defect the `mirrored_at` CASE exists for: an album that gains a
/// track and is interrupted before its fetch must not read as fresh.
#[tokio::test]
async fn a_changed_count_invalidates_the_walk_even_without_a_fetch() {
    let pool = pool().await;
    upsert(&pool, &album("al-1", 3)).await;
    sqlx::query("UPDATE remote_album SET mirrored_at = 1 WHERE remote_id = 'al-1'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(album_is_fresh(&pool, &album("al-1", 3)).await.unwrap());

    // The listing now says four, and nothing else happens — no fetch, no
    // stamp. The album must come back stale on the next walk.
    upsert(&pool, &album("al-1", 4)).await;
    assert!(!album_is_fresh(&pool, &album("al-1", 4)).await.unwrap());

    // Re-listing at the same count must not clear a stamp that is valid.
    sqlx::query("UPDATE remote_album SET mirrored_at = 2 WHERE remote_id = 'al-1'")
        .execute(&pool)
        .await
        .unwrap();
    upsert(&pool, &album("al-1", 4)).await;
    assert!(album_is_fresh(&pool, &album("al-1", 4)).await.unwrap());
}

/// An empty answer means "this account can browse nothing", not "the
/// server has nothing" — deleting on it discards every sweep date.
#[tokio::test]
async fn an_empty_library_answer_deletes_nothing() {
    let pool = pool().await;
    store_libraries(
        &pool,
        &[LibraryAccessDto {
            id: "lib-1".into(),
            name: "One".into(),
        }],
    )
    .await
    .unwrap();
    sqlx::query("UPDATE remote_library SET mirrored_at = 7")
        .execute(&pool)
        .await
        .unwrap();

    store_libraries(&pool, &[]).await.unwrap();

    let rows: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT remote_id, mirrored_at FROM remote_library")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![("lib-1".to_string(), Some(7))]);
}
