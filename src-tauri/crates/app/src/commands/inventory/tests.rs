use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// The repo's own profile migrations, with `foreign_keys` on.
///
/// A hand-written fixture would let these clauses pass against a
/// schema the app never has -- which is how `album.album_artist_id`
/// survived being written down at all: the column does not exist.
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

/// Four discs chosen for what they must *not* trigger:
///
/// - album 1 disc 3 numbered 20-22: complete, and the case that
///   `MAX > COUNT(DISTINCT)` reports as missing nineteen tracks;
/// - album 1 disc 1 numbered 1-2: proof the rule splits per disc
///   rather than per album;
/// - album 2 disc 1 numbered 1, 2, 4: the genuine hole;
/// - album 3 disc 1 with two tracks both claiming slot 8: a clash,
///   and deliberately not also a gap.
async fn seed(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'A', 'a')",
        "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (1, 'Box Set', 'box set', 1, 1999, 0)",
        "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (2, 'Gappy', 'gappy', 1, 2000, 0)",
        "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (3, 'Clashing', 'clashing', 1, 2001, 0)",
    ] {
        sqlx::raw_sql(statement).execute(pool).await.unwrap();
    }

    for (id, album, disc, number) in [
        (1i64, 1i64, 3i64, 20i64),
        (2, 1, 3, 21),
        (3, 1, 3, 22),
        (4, 1, 1, 1),
        (5, 1, 1, 2),
        (6, 2, 1, 1),
        (7, 2, 1, 2),
        (8, 2, 1, 4),
        (9, 3, 1, 8),
        (10, 3, 1, 8),
    ] {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, album_id, disc_number,
                                    track_number, primary_artist, duration_ms, added_at,
                                    is_available, hlc_wall, hlc_logical, rating_hlc_wall,
                                    rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, ?, ?, ?, ?, 1, 300000, 0, 1, 0, 0, 0, 0)",
        )
        .bind(id)
        .bind(format!("/m/{id}.flac"))
        .bind(format!("h{id}"))
        .bind(format!("T{id}"))
        .bind(album)
        .bind(disc)
        .bind(number)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO track_artist (track_id, artist_id, position) VALUES (?, 1, 0)")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
}

/// The ids one category holds, through the exact query the command
/// runs -- clause, wrapper and bound parameters alike.
async fn ids_for(pool: &SqlitePool, key: &str) -> Vec<i64> {
    let clause = where_for(key).expect("known category");
    let sql = format!(
        "SELECT CAST(id AS INTEGER) FROM ({}) ORDER BY 1",
        library_tracks_sql_where(clause, "")
    );
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
        .bind(Option::<i64>::None)
        .bind(Option::<i64>::None)
        .bind(Option::<i64>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_gap_is_measured_inside_the_observed_range() {
    let pool = pool().await;
    seed(&pool).await;
    // Album 2's disc only. A disc starting at 20 is complete, and a
    // disc whose only fault is a repeated number is not a hole.
    assert_eq!(ids_for(&pool, "track_number_gap").await, vec![6, 7, 8]);
}

/// The ends the gap rule cannot see, on top of the shared seed:
///
/// - album 4, numbered 2-5: its opener is missing;
/// - album 5, numbered 1-3 of a declared 4: its closer is missing;
/// - album 6, numbered 1-3 of a declared 3: complete.
///
/// From the seed, album 2 (1, 2, 4) is a hole and stays out — it is
/// the gap category's — album 3 (two tracks claiming 8) is one number
/// and is not judged, and album 1 is in: discs 1 and 3, so disc 2 is
/// missing.
#[tokio::test]
async fn missing_ends_are_found_by_the_numbers_and_the_totals() {
    let pool = pool().await;
    seed(&pool).await;
    for statement in [
        "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (4, 'No Opener', 'no opener', 1, 2002, 0)",
        "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (5, 'No Closer', 'no closer', 1, 2003, 0)",
        "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (6, 'Whole', 'whole', 1, 2004, 0)",
    ] {
        sqlx::raw_sql(statement).execute(&pool).await.unwrap();
    }
    for (id, album, number, total) in [
        (20i64, 4i64, 2i64, None::<i64>),
        (21, 4, 3, None),
        (22, 4, 4, None),
        (23, 4, 5, None),
        (24, 5, 1, Some(4)),
        (25, 5, 2, Some(4)),
        (26, 5, 3, Some(4)),
        (27, 6, 1, Some(3)),
        (28, 6, 2, Some(3)),
        (29, 6, 3, Some(3)),
    ] {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, album_id, track_number,
                                    track_total, primary_artist, duration_ms, added_at,
                                    is_available, hlc_wall, hlc_logical, rating_hlc_wall,
                                    rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, ?, ?, ?, ?, 1, 300000, 0, 1, 0, 0, 0, 0)",
        )
        .bind(id)
        .bind(format!("/m/{id}.flac"))
        .bind(format!("h{id}"))
        .bind(format!("T{id}"))
        .bind(album)
        .bind(number)
        .bind(total)
        .execute(&pool)
        .await
        .unwrap();
    }
    assert_eq!(
        albums_missing_ends_ids(&pool).await.unwrap(),
        vec![1, 2, 3, 4, 5, 20, 21, 22, 23, 24, 25, 26]
    );

    // The album page's reading of the same rows.
    let opener = album_completeness_of(&pool, 4).await.unwrap();
    assert_eq!(opener.discs[0].tracks, [1]);
    let closer = album_completeness_of(&pool, 5).await.unwrap();
    assert_eq!(closer.discs[0].tracks, [4]);
    assert!(album_completeness_of(&pool, 6).await.unwrap().is_complete());
    let gap = album_completeness_of(&pool, 2).await.unwrap();
    assert_eq!(gap.discs[0].tracks, [3]);
    assert!(!gap.missing_at_the_ends());
}

/// The SQL half of the probable-duplicate rule: what lands in a
/// bucket together. The chaining itself is tested in
/// `waveflow_core::inventory`; what is tested here is that two
/// recordings reach the same bucket at all.
async fn seed_duplicates(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'A', 'a')",
        "INSERT INTO artist (id, name, canonical_name) VALUES (2, 'B', 'b')",
    ] {
        sqlx::raw_sql(statement).execute(pool).await.unwrap();
    }

    // 1-3: one recording three times, 180 s / 181.5 s / 183 s. The
    // outer pair is 3 s apart, so a pairwise tolerance splits them;
    // the chain must not. Their titles differ only by the case and
    // punctuation `normalize_name` folds, and their two artists are
    // credited in opposite orders -- the bucket key sorts by artist
    // id, so a credit is a set and not a display string.
    //
    // 4-5: no credited artist at all, same title and length. Two
    // untagged copies of one file are exactly what this should
    // surface, so they share the empty-credit bucket rather than
    // being skipped.
    //
    // 6: same title, far too long to be the same recording.
    let tracks: &[(i64, &str, i64, &[(i64, i64)])] = &[
        (1, "Blue Monday", 180_000, &[(1, 0), (2, 1)]),
        (2, "blue monday!", 181_500, &[(2, 0), (1, 1)]),
        (3, "Blue  Monday", 183_000, &[(1, 0), (2, 1)]),
        (4, "Untitled", 200_000, &[]),
        (5, "Untitled", 200_500, &[]),
        (6, "Blue Monday", 400_000, &[(1, 0)]),
    ];
    for (id, title, duration, credits) in tracks {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, duration_ms, added_at,
                                    is_available, hlc_wall, hlc_logical, rating_hlc_wall,
                                    rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, ?, ?, 0, 1, 0, 0, 0, 0)",
        )
        .bind(id)
        .bind(format!("/d/{id}.flac"))
        .bind(format!("d{id}"))
        .bind(title)
        .bind(duration)
        .execute(pool)
        .await
        .unwrap();
        for (artist_id, position) in *credits {
            sqlx::query(
                "INSERT INTO track_artist (track_id, artist_id, position)
                     VALUES (?, ?, ?)",
            )
            .bind(id)
            .bind(artist_id)
            .bind(position)
            .execute(pool)
            .await
            .unwrap();
        }
    }
}

/// Both shapes of "no title", and nothing else.
#[tokio::test]
async fn an_untitled_file_is_found_whichever_way_it_is_untitled() {
    let pool = pool().await;
    sqlx::raw_sql(
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    // 1: properly titled. 2: the title tag was there and empty.
    // 3: no title tag at all, so the scanner wrote the file stem --
    // the common case, and the one a text test cannot see.
    for (id, title, from_filename) in [
        (1i64, "Real Title", 0i64),
        (2, "", 0),
        (3, "03 - untagged", 1),
    ] {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, title_from_filename, duration_ms,
                                    added_at, is_available, hlc_wall, hlc_logical,
                                    rating_hlc_wall, rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, ?, ?, 1000, 0, 1, 0, 0, 0, 0)",
        )
        .bind(id)
        .bind(format!("/t/{id}.flac"))
        .bind(format!("t{id}"))
        .bind(title)
        .bind(from_filename)
        .execute(&pool)
        .await
        .unwrap();
    }

    assert_eq!(ids_for(&pool, "missing_title").await, vec![2, 3]);
}

#[tokio::test]
async fn probable_duplicates_chain_and_ignore_credit_order() {
    let pool = pool().await;
    seed_duplicates(&pool).await;
    let ids = probable_duplicate_ids(&pool).await.unwrap();
    assert_eq!(ids, vec![1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn a_clash_is_scoped_to_one_disc_of_one_album() {
    let pool = pool().await;
    seed(&pool).await;
    // Both sides of the clash, and nothing from the box set -- whose
    // discs 1 and 3 each hold a track numbered differently but would
    // collide if the check grouped by album alone.
    assert_eq!(ids_for(&pool, "duplicate_track_number").await, vec![9, 10]);
}

/// #719, against the real migrations. Four comma names:
///
/// - `Ice Spice, Central Cee`, one fragment already an artist: listed
///   first, with that fragment linked;
/// - `Drake, Lil Durk`, no fragment in the library: still listed —
///   measured on a real library, that is what most real duos look like;
/// - `Tyler, The Creator`, dismissed by the user: not listed;
/// - `Solo,` which splits into one name only: not a split at all.
///
/// And `Unplayed, Duo`, credited on no available track: not listed.
#[tokio::test]
async fn comma_names_are_listed_unless_dismissed_or_unsplittable() {
    let pool = pool().await;
    sqlx::raw_sql(
        r#"INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0);
             INSERT INTO artist (id, name, canonical_name) VALUES
                 (1, 'Ice Spice, Central Cee', 'ice spice central cee'),
                 (2, 'Ice Spice', 'ice spice'),
                 (3, 'Drake, Lil Durk', 'drake lil durk'),
                 (4, 'Tyler, The Creator', 'tyler the creator'),
                 (5, 'Solo,', 'solo'),
                 (6, 'Unplayed, Duo', 'unplayed duo');
             INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('inventory.dismissed_phantoms', '["tyler the creator"]', 'json', 0);"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    for (id, artist, available) in [
        (1i64, 1i64, 1i64),
        (2, 1, 1),
        (3, 3, 1),
        (4, 4, 1),
        (5, 5, 1),
        (6, 6, 0),
    ] {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, primary_artist, duration_ms,
                                    added_at, is_available, hlc_wall, hlc_logical,
                                    rating_hlc_wall, rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, 'T', ?, 300000, 0, ?, 0, 0, 0, 0)",
        )
        .bind(id)
        .bind(format!("/p/{id}.flac"))
        .bind(format!("p{id}"))
        .bind(artist)
        .bind(available)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO track_artist (track_id, artist_id, position) VALUES (?, ?, 0)")
            .bind(id)
            .bind(artist)
            .execute(&pool)
            .await
            .unwrap();
    }

    let listed = phantom_artists(&pool).await.unwrap();
    let names: Vec<&str> = listed.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["Ice Spice, Central Cee", "Drake, Lil Durk"]);
    assert_eq!(listed[0].track_count, 2);
    assert_eq!(listed[0].fragments[0].artist_id, Some(2));
    assert_eq!(listed[0].fragments[1].artist_id, None);
}

/// #851: a Chinese credit joins two names with `、`, or with the
/// fullwidth `，` a CJK keyboard types for a comma. Neither reached the
/// list when only the ASCII comma counted.
#[tokio::test]
async fn names_joined_by_cjk_commas_are_listed() {
    let pool = pool().await;
    sqlx::raw_sql(
        r#"INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0);"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    for (id, name) in [(1i64, "庄心妍、祁隆"), (2, "蔡依林，周杰伦"), (3, "蔡依林")]
    {
        sqlx::query("INSERT INTO artist (id, name, canonical_name) VALUES (?, ?, ?)")
            .bind(id)
            .bind(name)
            .bind(canonical_name(name))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, primary_artist, duration_ms,
                                    added_at, is_available, hlc_wall, hlc_logical,
                                    rating_hlc_wall, rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, 'T', ?, 300000, 0, 1, 0, 0, 0, 0)",
        )
        .bind(id)
        .bind(format!("/c/{id}.flac"))
        .bind(format!("c{id}"))
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO track_artist (track_id, artist_id, position) VALUES (?, ?, 0)")
            .bind(id)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    let listed = phantom_artists(&pool).await.unwrap();
    let names: Vec<&str> = listed.iter().map(|p| p.name.as_str()).collect();
    // The one with a fragment already in the library sorts first.
    assert_eq!(names, vec!["蔡依林，周杰伦", "庄心妍、祁隆"]);
    let fragments: Vec<&str> = listed[1]
        .fragments
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(fragments, vec!["庄心妍", "祁隆"]);
    assert_eq!(listed[0].fragments[0].artist_id, Some(3));
}
