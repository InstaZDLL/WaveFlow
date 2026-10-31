use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;
use waveflow_core::scanner::upsert_album;

/// The real migrator, `foreign_keys` on: what `sync_db` writes must
/// hold against the schema the app actually has.
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
    for statement in [
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'pink floyd', 'pink floyd')",
        "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (1, 'the wall', 'the wall', 1, 1979, 0)",
        "INSERT INTO genre (id, name, canonical_name) VALUES (1, 'prog rock', 'prog rock')",
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available, year,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/m/1.flac', 'h1', 1, 0, 'In the Flesh?', 1, 1000, 0, 1, 1979,
                     0, 0, 0, 0)",
        "UPDATE track SET primary_artist = 1 WHERE id = 1",
        "INSERT INTO track_artist (track_id, artist_id, position) VALUES (1, 1, 0)",
        "INSERT INTO track_genre (track_id, genre_id) VALUES (1, 1)",
    ] {
        sqlx::raw_sql(statement).execute(&pool).await.unwrap();
    }
    pool
}

fn edit() -> TrackEdit {
    TrackEdit {
        title: None,
        artist: None,
        album: None,
        year: None,
        track_number: None,
        disc_number: None,
        genre: None,
        album_artist: None,
        composer: None,
        track_total: None,
        disc_total: None,
        musicbrainz: MusicBrainzEdit::default(),
    }
}

/// `(album id, album artist id, album_artist text, is_compilation)`
/// of the album track `id` sits in.
async fn album_of(pool: &SqlitePool, id: i64) -> (i64, Option<i64>, Option<String>, i64) {
    sqlx::query_as(
        "SELECT al.id, al.artist_id, al.album_artist, al.is_compilation
               FROM track t JOIN album al ON al.id = t.album_id
              WHERE t.id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A second track of "the wall", and a cover on the album.
async fn with_a_second_track_and_a_cover(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO artwork (id, hash, format, source, created_at)
             VALUES (1, 'cover', 'jpg', 'embedded', 0)",
        "UPDATE album SET artwork_id = 1, artwork_source = 'embedded' WHERE id = 1",
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available, year,
                                track_number, hlc_wall, hlc_logical, rating_hlc_wall,
                                rating_hlc_logical)
             VALUES (2, 1, '/m/2.flac', 'h2', 1, 0, 'The Thin Ice', 1, 1000, 0, 1, 1979, 2,
                     0, 0, 0, 0)",
        "UPDATE track SET primary_artist = 1 WHERE id = 2",
        "INSERT INTO track_artist (track_id, artist_id, position) VALUES (2, 1, 0)",
    ] {
        sqlx::raw_sql(statement).execute(pool).await.unwrap();
    }
}

/// An Album Artist is part of what an album is, so setting one moves
/// the track — into the album a rescan of the file would build, and the
/// rest of the record follows it there rather than into a second one.
#[tokio::test]
async fn an_album_artist_moves_the_track_to_that_album() {
    let pool = pool().await;
    with_a_second_track_and_a_cover(&pool).await;
    let fix = TrackEdit {
        album_artist: Some("Roger Waters".into()),
        ..edit()
    };

    sync_db(&pool, 1, &fix).await.unwrap();
    let (moved, artist_id, text, _) = album_of(&pool, 1).await;
    assert_ne!(moved, 1, "a new album artist is a new album");
    let name: String = sqlx::query_scalar("SELECT name FROM artist WHERE id = ?")
        .bind(artist_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Roger Waters");
    assert_eq!(text.as_deref(), Some("Roger Waters"));
    assert_eq!(album_of(&pool, 2).await.0, 1, "the other track stays");
    let (title, cover, year): (String, Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT title, artwork_id, year FROM album WHERE id = ?")
            .bind(moved)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(title, "the wall", "the title is the album's own");
    assert_eq!(cover, Some(1), "the new album took the old one's cover");
    assert_eq!(year, Some(1979));

    sync_db(&pool, 2, &fix).await.unwrap();
    assert_eq!(album_of(&pool, 2).await.0, moved, "one album, not two");

    // The same album a scan of the retagged file resolves.
    let mut conn = pool.acquire().await.unwrap();
    let scanned = upsert_album(
        &mut conn,
        "the wall",
        Some("Roger Waters"),
        false,
        Some(1),
        None,
    )
    .await
    .unwrap();
    assert_eq!(scanned, Some(moved));
}

/// Emptied, the grouping falls back as the scanner's does: back to the
/// track's first artist — the album it came from.
#[tokio::test]
async fn clearing_the_album_artist_falls_back_to_the_first_artist() {
    let pool = pool().await;
    sync_db(
        &pool,
        1,
        &TrackEdit {
            album_artist: Some("Roger Waters".into()),
            ..edit()
        },
    )
    .await
    .unwrap();
    assert_ne!(album_of(&pool, 1).await.0, 1);

    sync_db(
        &pool,
        1,
        &TrackEdit {
            album_artist: Some("  ".into()),
            ..edit()
        },
    )
    .await
    .unwrap();
    assert_eq!(album_of(&pool, 1).await, (1, Some(1), None, 0));
}

/// A named Album Artist does not inherit the compilation flag of the
/// album the track left — no scan of the retagged file would set it,
/// and it is sticky wherever it lands.
#[tokio::test]
async fn a_named_album_artist_leaves_the_compilation_flag_behind() {
    let pool = pool().await;
    sqlx::raw_sql("UPDATE album SET is_compilation = 1 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    sync_db(
        &pool,
        1,
        &TrackEdit {
            album_artist: Some("Various".into()),
            ..edit()
        },
    )
    .await
    .unwrap();
    let (id, _, text, compilation) = album_of(&pool, 1).await;
    assert_ne!(id, 1);
    assert_eq!(text.as_deref(), Some("Various"));
    assert_eq!(compilation, 0);
    let left: i64 = sqlx::query_scalar("SELECT is_compilation FROM album WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 1, "the album left keeps its own flag");
}

/// A rename moves the track into another record, whose cover is not
/// the one of the album it left.
#[tokio::test]
async fn a_rename_does_not_carry_the_cover() {
    let pool = pool().await;
    with_a_second_track_and_a_cover(&pool).await;
    sync_db(
        &pool,
        1,
        &TrackEdit {
            album: Some("Animals".into()),
            ..edit()
        },
    )
    .await
    .unwrap();
    let (moved, ..) = album_of(&pool, 1).await;
    assert_ne!(moved, 1);
    let cover: Option<i64> = sqlx::query_scalar("SELECT artwork_id FROM album WHERE id = ?")
        .bind(moved)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cover, None);
}

/// Renaming the album and setting its artist in one save lands on the
/// album both name, and an existing cover is never overwritten.
#[tokio::test]
async fn a_rename_and_an_album_artist_in_one_save() {
    let pool = pool().await;
    with_a_second_track_and_a_cover(&pool).await;
    sqlx::raw_sql(
        "INSERT INTO artwork (id, hash, format, source, created_at)
             VALUES (2, 'other', 'jpg', 'embedded', 0);
             INSERT INTO artist (id, name, canonical_name) VALUES (2, 'Roger Waters', 'roger waters');
             INSERT INTO album (id, title, canonical_title, artist_id, album_artist,
                                is_compilation, artwork_id)
             VALUES (2, 'The Wall Live', 'the wall live', 2, 'Roger Waters', 0, 2);",
    )
    .execute(&pool)
    .await
    .unwrap();
    sync_db(
        &pool,
        1,
        &TrackEdit {
            album: Some("The Wall (Live)".into()),
            album_artist: Some("roger waters".into()),
            ..edit()
        },
    )
    .await
    .unwrap();
    let (id, artist_id, text, _) = album_of(&pool, 1).await;
    assert_eq!(
        (id, artist_id),
        (2, Some(2)),
        "the existing album both name"
    );
    // The spelling typed reaches the album and its artist.
    assert_eq!(text.as_deref(), Some("roger waters"));
    let cover: Option<i64> = sqlx::query_scalar("SELECT artwork_id FROM album WHERE id = 2")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cover, Some(2), "an existing album keeps its cover");
}

/// A track in no album has nothing to regroup: the value is the file's.
#[tokio::test]
async fn an_album_artist_on_a_track_without_an_album_moves_nothing() {
    let pool = pool().await;
    sqlx::raw_sql("UPDATE track SET album_id = NULL WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    sync_db(
        &pool,
        1,
        &TrackEdit {
            album_artist: Some("Roger Waters".into()),
            ..edit()
        },
    )
    .await
    .unwrap();
    let album: Option<i64> = sqlx::query_scalar("SELECT album_id FROM track WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(album, None);
    let albums: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(albums, 1, "no album was created for it");
}

/// The composer lands in the row the scanner fills, whatever case an
/// earlier scan stored its key in, and clearing it removes the row.
#[tokio::test]
async fn the_composer_reaches_the_scanned_row() {
    let pool = pool().await;
    sqlx::raw_sql(
        "INSERT INTO track_tag (track_id, key, value) VALUES (1, 'Composer', 'old');
             INSERT INTO track_tag (track_id, key, value) VALUES (1, 'ISRC', 'GBN9Y1100088');",
    )
    .execute(&pool)
    .await
    .unwrap();
    let tags = |pool: SqlitePool| async move {
        sqlx::query_as::<_, (String, String)>(
            "SELECT key, value FROM track_tag WHERE track_id = 1 ORDER BY key",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };

    sync_db(
        &pool,
        1,
        &TrackEdit {
            composer: Some("  Roger Waters ".into()),
            ..edit()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tags(pool.clone()).await,
        [
            ("COMPOSER".to_string(), "Roger Waters".to_string()),
            ("ISRC".to_string(), "GBN9Y1100088".to_string()),
        ]
    );

    sync_db(
        &pool,
        1,
        &TrackEdit {
            composer: Some(" ".into()),
            ..edit()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tags(pool.clone()).await,
        [("ISRC".to_string(), "GBN9Y1100088".to_string())]
    );
}

/// Case and punctuation fixes resolve to the same rows through the
/// canonical key; they used to leave the old spelling on display.
#[tokio::test]
async fn a_respelled_name_reaches_the_existing_row() {
    let pool = pool().await;
    let fix = TrackEdit {
        artist: Some("Pink Floyd".into()),
        album: Some("The Wall".into()),
        genre: Some("Prog Rock".into()),
        ..edit()
    };
    sync_db(&pool, 1, &fix).await.unwrap();

    let artist: (i64, String) =
        sqlx::query_as("SELECT ar.id, ar.name FROM track t JOIN artist ar ON ar.id = t.primary_artist WHERE t.id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(artist, (1, "Pink Floyd".to_string()));
    let album: (i64, String) = sqlx::query_as(
        "SELECT al.id, al.title FROM track t JOIN album al ON al.id = t.album_id WHERE t.id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(album, (1, "The Wall".to_string()));
    let genre: (i64, String) = sqlx::query_as(
        "SELECT g.id, g.name FROM track_genre tg JOIN genre g ON g.id = tg.genre_id WHERE tg.track_id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(genre, (1, "Prog Rock".to_string()));
}

/// Several genres become several rows, the existing one reused through
/// its canonical key; clearing removes them all (#729).
#[tokio::test]
async fn several_genres_become_several_rows() {
    let pool = pool().await;
    let genres = |pool: SqlitePool| async move {
        sqlx::query_scalar::<_, String>(
            "SELECT g.name FROM track_genre tg JOIN genre g ON g.id = tg.genre_id
                  WHERE tg.track_id = 1 ORDER BY g.name",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };

    let tagged = TrackEdit {
        genre: Some("Prog Rock; Art Rock;prog rock".into()),
        ..edit()
    };
    sync_db(&pool, 1, &tagged).await.unwrap();
    assert_eq!(genres(pool.clone()).await, ["Art Rock", "Prog Rock"]);
    let reused: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_genre WHERE track_id = 1 AND genre_id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reused, 1, "the existing genre row is the one linked");

    let cleared = TrackEdit {
        genre: Some(" ; ".into()),
        ..edit()
    };
    sync_db(&pool, 1, &cleared).await.unwrap();
    assert!(genres(pool.clone()).await.is_empty());
}

/// `"; "` separates artists; a comma stays inside a name.
#[tokio::test]
async fn artists_split_on_the_semicolon_only() {
    let pool = pool().await;
    let credits = TrackEdit {
        artist: Some("Earth, Wind & Fire; The Emotions".into()),
        ..edit()
    };
    sync_db(&pool, 1, &credits).await.unwrap();

    let names: Vec<String> = sqlx::query_scalar(
        "SELECT ar.name FROM track_artist ta JOIN artist ar ON ar.id = ta.artist_id
              WHERE ta.track_id = 1 ORDER BY ta.position",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(names, ["Earth, Wind & Fire", "The Emotions"]);
}

/// The album header took its year from the scan alone.
#[tokio::test]
async fn an_edited_year_reaches_the_album() {
    let pool = pool().await;
    let year = TrackEdit {
        year: Some(1980),
        ..edit()
    };
    sync_db(&pool, 1, &year).await.unwrap();

    let album_year: Option<i64> = sqlx::query_scalar("SELECT year FROM album WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(album_year, Some(1980));
}
