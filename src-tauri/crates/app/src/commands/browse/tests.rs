use super::{library_track_order_clause, track_sort_expr};

/// The security property, not a style one: `order_by` arrives from
/// the frontend and the result is handed to `AssertSqlSafe`, so
/// anything outside the whitelist has to fall back to the default
/// clause rather than reach a query (#588).
#[test]
fn an_unknown_sort_key_falls_back_to_the_default_order() {
    for key in [
        "nonsense",
        "title; DROP TABLE track",
        "1) UNION SELECT 1 --",
        "",
        "TITLE",
    ] {
        let clause = library_track_order_clause(Some(key), Some("asc"));
        assert!(
            clause.starts_with("ORDER BY sort_artist COLLATE NAME"),
            "{key:?} produced {clause:?}"
        );
        assert!(!clause.contains(key) || key.is_empty(), "{key:?} leaked");
    }
}

/// Every sortable column must be reachable. The previous shape spelled
/// out two match arms per column and quietly stopped at seven, which is
/// what this pins.
#[test]
fn every_column_the_table_can_show_is_sortable() {
    for key in [
        "title",
        "artist",
        "album",
        "duration_ms",
        "year",
        "added_at",
        "rating",
        "track_number",
        "disc_number",
        "bitrate",
        "sample_rate",
        "bit_depth",
        "channels",
        "codec",
        "musical_key",
        "file_size",
        "file_path",
    ] {
        assert!(track_sort_expr(key).is_some(), "{key} is not sortable");
        let clause = library_track_order_clause(Some(key), Some("asc"));
        assert!(
            !clause.starts_with("ORDER BY sort_artist COLLATE NAME,"),
            "{key} fell through to the default"
        );
    }
}

/// An untagged track is not a track with the smallest value. A run of
/// blanks at the top of an ascending sort is the fastest way to make a
/// column look broken.
#[test]
fn nullable_columns_sort_their_blanks_last_in_both_directions() {
    for dir in ["asc", "desc"] {
        let clause = library_track_order_clause(Some("bitrate"), Some(dir));
        assert!(clause.contains("bitrate IS NULL"), "{clause}");
    }
    // Not nullable, so no guard — `duration_ms` is `NOT NULL`.
    let clause = library_track_order_clause(Some("duration_ms"), Some("asc"));
    assert!(!clause.contains("IS NULL"), "{clause}");
}

/// Clicking "added" or "duration" for the first time should give the
/// newest and the longest, not the oldest and the shortest.
#[test]
fn some_columns_default_to_descending() {
    assert!(library_track_order_clause(Some("added_at"), None).contains("DESC"));
    assert!(library_track_order_clause(Some("rating"), None).contains("DESC"));
    assert!(library_track_order_clause(Some("title"), None).contains("ASC"));
    assert!(library_track_order_clause(Some("codec"), None).contains("ASC"));
}

/// Two rows that compare equal must not swap places between two
/// identical queries — a list that reshuffles on every refresh is a
/// list nobody trusts.
#[test]
fn every_sort_carries_a_tie_break() {
    for key in ["artist", "year", "bitrate", "codec"] {
        let clause = library_track_order_clause(Some(key), Some("asc"));
        assert!(
            clause.contains("title COLLATE FOLD"),
            "{key} has no tie-break: {clause}"
        );
    }
    // An album sort reads down the disc instead -- but only after
    // the artist, or two albums sharing a title (`Greatest Hits`)
    // interleave their track 1s.
    let album = library_track_order_clause(Some("album"), Some("asc"));
    assert!(
        album.contains("sort_artist COLLATE NAME, disc_number, track_number"),
        "{album}"
    );
}

use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

/// The real migrator against a real database, `foreign_keys` on — the only
/// fixture that proves a compound select over fourteen tables actually
/// runs. The unified listings join the attached `app` database too, so the
/// fixture attaches one and creates the single table they read.
async fn pool() -> SqlitePool {
    let options = waveflow_core::repository::sqlite::collation::register(
        SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true),
    );
    let pool = SqlitePoolOptions::new()
        // One connection: `ATTACH` is per-connection, and a second one
        // would not see the attached database.
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("ATTACH DATABASE ':memory:' AS app")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE app.metadata_artist (
                deezer_id    INTEGER PRIMARY KEY,
                picture_url  TEXT,
                picture_hash TEXT
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// One local artist with one album and one track, and one server artist
/// with one album and two tracks. The two artists share a name that only
/// the normaliser makes comparable.
async fn seed(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'Björk', 'bjork')",
        "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (1, 'Vespertine', 'vespertine', 1, 2001, 0)",
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/m/1.flac', 'h1', 1, 0, 'T1', 1, 300000, 500, 1, 0, 0, 0, 0)",
        // The scanner always stamps a primary artist; a fixture that omits
        // it would exercise a shape the library never holds.
        "UPDATE track SET primary_artist = 1 WHERE id = 1",
        "INSERT INTO track_artist (track_id, artist_id, position) VALUES (1, 1, 0)",
        "INSERT INTO remote_artist (remote_id, name, artwork_hash, sort_key, mirrored_at)
             VALUES ('ar-1', 'Aphex Twin', 'aa11', 'aphex twin', 1)",
        "INSERT INTO remote_album (remote_id, title, artist, artist_id, song_count,
                                       duration_ms, created_at, sort_title, sort_artist)
             VALUES ('al-1', 'Drukqs', 'Aphex Twin', 'ar-1', 2, 1000, 100, 'drukqs',
                     'aphex twin')",
        "INSERT INTO remote_track (remote_id, title, artist_id, duration_ms, cached_at,
                                       in_catalogue)
             VALUES ('t-1', 'R1', 'ar-1', 0, 1, 1)",
        "INSERT INTO remote_track (remote_id, title, artist_id, duration_ms, cached_at,
                                       in_catalogue)
             VALUES ('t-2', 'R2', 'ar-1', 0, 1, 1)",
        // Fully described, so the track listing has something to sort and
        // render: the two above are bare identifiers on purpose.
        "UPDATE remote_track
                SET artist = 'Aphex Twin', album = 'Drukqs', album_id = 'al-1',
                    sort_artist = 'aphex twin', sort_album = 'drukqs',
                    track_no = 1, disc_no = 1, artwork_hash = 'bb22'
              WHERE remote_id IN ('t-1', 't-2')",
        // Cached for a playlist but never walked: outside the catalogue,
        // so the library must not list it.
        "INSERT INTO remote_track (remote_id, title, artist, duration_ms, cached_at,
                                       in_catalogue)
             VALUES ('t-3', 'Not in the catalogue', 'Someone', 0, 1, 0)",
    ] {
        sqlx::raw_sql(statement).execute(pool).await.unwrap();
    }
}

// ── Folder browsing (#578) ──────────────────────────────────────

/// Fixture paths are built with the platform separator, because the
/// production helpers use it: a test hard-coding `/` would pass on
/// Linux and describe nothing on Windows.
fn p(parts: &[&str]) -> String {
    parts.join(std::path::MAIN_SEPARATOR_STR)
}

/// A root with two albums, a nested disc folder, and a stray file at
/// the top. `foreign` sits outside the root and must never appear.
async fn seed_folders(pool: &SqlitePool) {
    let root = p(&["", "m", "Rock"]);
    let files = [
        (10, p(&[&root, "top.flac"]), 100),
        (11, p(&[&root, "Album A", "1.flac"]), 200),
        (12, p(&[&root, "Album A", "2.flac"]), 300),
        (13, p(&[&root, "Album B", "Disc 1", "1.flac"]), 400),
        (14, p(&[&root, "Album B", "Disc 2", "1.flac"]), 500),
        // Same prefix up to a character: proof the band stops at the
        // separator instead of matching any string that starts the same.
        (15, p(&["", "m", "Rockabilly", "1.flac"]), 600),
        (16, p(&["", "m", "Jazz", "1.flac"]), 700),
    ];
    sqlx::raw_sql(
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_folder (id, library_id, path, last_scanned_at, is_watched)
             VALUES (1, 1, ?, 0, 0)",
    )
    .bind(&root)
    .execute(pool)
    .await
    .unwrap();
    // One artwork, on Album A only: the cover rule has to pick it for
    // that folder and leave the others without one.
    for statement in [
        "INSERT INTO artwork (id, hash, format, width, height, source, created_at)
             VALUES (1, 'aa11', 'jpg', 10, 10, 'embedded', 0)",
        "INSERT INTO album (id, title, canonical_title, artwork_id, is_compilation)
             VALUES (1, 'Album A', 'album a', 1, 0)",
    ] {
        sqlx::raw_sql(statement).execute(pool).await.unwrap();
    }
    for (id, path, size) in files {
        sqlx::query(
            "INSERT INTO track (id, library_id, folder_id, file_path, file_hash, file_size,
                                    file_modified, title, album_id, duration_ms, added_at,
                                    is_available, hlc_wall, hlc_logical,
                                    rating_hlc_wall, rating_hlc_logical)
                 VALUES (?, 1, 1, ?, 'h', ?, 0, 'T', ?, 1000, 0, 1, 0, 0, 0, 0)",
        )
        .bind(id)
        .bind(&path)
        .bind(size)
        .bind(if path.contains("Album A") {
            Some(1_i64)
        } else {
            None
        })
        .execute(pool)
        .await
        .unwrap();
    }
}

/// The children query, bound exactly as `browse_folders` binds it.
async fn children(pool: &SqlitePool, dir: &str) -> Vec<(String, i64, i64, Option<String>)> {
    let (low, high) = folder_prefix_range(dir);
    let sep = std::path::MAIN_SEPARATOR.to_string();
    sqlx::query(FOLDER_CHILDREN_SQL)
        .bind(low.chars().count() as i64)
        .bind(Some(1_i64))
        .bind(Some(1_i64))
        .bind(&low)
        .bind(&high)
        .bind(&sep)
        .bind(&sep)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.get("name"),
                row.get("track_count"),
                row.get("total_size"),
                row.get("artwork_hash"),
            )
        })
        .collect()
}

#[test]
fn a_folder_band_stops_at_the_separator() {
    let (low, high) = folder_prefix_range(&p(&["", "m", "Rock"]));
    assert!(low.ends_with(std::path::MAIN_SEPARATOR));
    // The whole point: the band must not swallow a sibling whose name
    // merely starts the same way.
    let sibling = p(&["", "m", "Rockabilly", "1.flac"]);
    assert!(
        !(sibling >= low && sibling < high),
        "Rockabilly is not under Rock"
    );
    let inside = p(&["", "m", "Rock", "1.flac"]);
    assert!(inside >= low && inside < high);
}

#[test]
fn a_trailing_separator_is_not_doubled() {
    let bare = folder_prefix_range(&p(&["", "m", "Rock"]));
    let trailing = folder_prefix_range(&format!(
        "{}{}",
        p(&["", "m", "Rock"]),
        std::path::MAIN_SEPARATOR
    ));
    assert_eq!(bare, trailing);
}

#[test]
fn the_parent_stops_at_the_library_root() {
    let root = p(&["", "m", "Rock"]);
    let album = p(&[&root, "Album A"]);
    assert_eq!(
        folder_parent(&album, Some(&root)).as_deref(),
        Some(&root[..])
    );
    // At the root itself there is nowhere to go: walking up would list
    // directories the scanner never indexed.
    assert!(folder_parent(&root, Some(&root)).is_none());
    // And with a trailing separator on either side, same answer.
    let with_sep = format!("{root}{}", std::path::MAIN_SEPARATOR);
    assert!(folder_parent(&with_sep, Some(&root)).is_none());
}

/// The root lookup, bound exactly as `browse_folders` binds it.
async fn root_for(pool: &SqlitePool, dir: &str) -> Option<(i64, String)> {
    let sep = std::path::MAIN_SEPARATOR_STR;
    sqlx::query_as(FOLDER_ROOT_SQL)
        .bind(Some(1_i64))
        .bind(Some(1_i64))
        .bind(dir)
        .bind(sep)
        .bind(sep)
        .bind(sep)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_root_stored_with_a_trailing_separator_still_matches() {
    let pool = pool().await;
    seed_folders(&pool).await;
    // A whole volume is `E:\` on Windows, and a folder picker can
    // hand back a trailing slash anywhere. Without the trim the
    // comparison appends a second separator, finds no root, and the
    // browser loses both its breadcrumb anchor and its stopping
    // point on the way up.
    let with_sep = format!("{}{}", p(&["", "m", "Jazz"]), std::path::MAIN_SEPARATOR);
    sqlx::query(
        "INSERT INTO library_folder (id, library_id, path, last_scanned_at, is_watched)
             VALUES (2, 1, ?, 0, 0)",
    )
    .bind(&with_sep)
    .execute(&pool)
    .await
    .unwrap();
    let found = root_for(&pool, &p(&["", "m", "Jazz", "Miles"])).await;
    assert_eq!(found.map(|(id, _)| id), Some(2));
}

#[tokio::test]
async fn the_nearest_root_wins_when_two_are_nested() {
    let pool = pool().await;
    seed_folders(&pool).await;
    // Both a parent and a child can be configured roots; the
    // breadcrumb has to stop at the nearer one.
    sqlx::query(
        "INSERT INTO library_folder (id, library_id, path, last_scanned_at, is_watched)
             VALUES (2, 1, ?, 0, 0)",
    )
    .bind(p(&["", "m", "Rock", "Album A"]))
    .execute(&pool)
    .await
    .unwrap();
    let found = root_for(&pool, &p(&["", "m", "Rock", "Album A", "Live"])).await;
    assert_eq!(found.map(|(id, _)| id), Some(2));
}

#[tokio::test]
async fn only_the_directories_one_level_down_are_listed() {
    let pool = pool().await;
    seed_folders(&pool).await;
    let root = p(&["", "m", "Rock"]);
    let rows = children(&pool, &root).await;
    let names: Vec<&str> = rows.iter().map(|(n, ..)| n.as_str()).collect();
    // "Disc 1" and "Disc 2" are two levels down, and `top.flac` is a
    // file, not a directory.
    assert_eq!(names, vec!["Album A", "Album B"]);
}

#[tokio::test]
async fn a_folder_counts_everything_beneath_it() {
    let pool = pool().await;
    seed_folders(&pool).await;
    let rows = children(&pool, &p(&["", "m", "Rock"])).await;
    let counts: Vec<(String, i64, i64)> = rows
        .iter()
        .map(|(n, c, s, _)| (n.clone(), *c, *s))
        .collect();
    assert_eq!(
        counts,
        vec![
            ("Album A".to_string(), 2, 500),
            // Both discs, from two levels down: recursive, which is
            // what makes the count meaningful on a parent folder.
            ("Album B".to_string(), 2, 900),
        ]
    );
}

#[tokio::test]
async fn folder_cover_comes_from_the_first_file() {
    let pool = pool().await;
    seed_folders(&pool).await;
    let rows = children(&pool, &p(&["", "m", "Rock"])).await;
    // Bare columns beside `MIN(rest)` take the minimum row's values.
    // This pins that SQLite guarantee, which the query depends on.
    assert_eq!(rows[0].3.as_deref(), Some("aa11"));
    assert_eq!(rows[1].3, None, "Album B has no artwork to borrow");
}

#[tokio::test]
async fn a_sibling_root_is_never_mixed_in() {
    let pool = pool().await;
    seed_folders(&pool).await;
    let rows = children(&pool, &p(&["", "m"])).await;
    let names: Vec<&str> = rows.iter().map(|(n, ..)| n.as_str()).collect();
    // Listing the parent of the root shows all three, each with its
    // own subtree count — and `Rockabilly` is its own folder, not part
    // of `Rock`.
    assert_eq!(names, vec!["Jazz", "Rock", "Rockabilly"]);
    assert_eq!(
        rows[1].1, 5,
        "Rock holds five tracks, Rockabilly's is not one"
    );
    assert_eq!(rows[2].1, 1);
}

#[tokio::test]
async fn a_folder_listing_can_be_direct_or_recursive() {
    let pool = pool().await;
    seed_folders(&pool).await;
    let root = p(&["", "m", "Rock"]);
    let (low, high) = folder_prefix_range(&root);
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let sql = library_tracks_sql_where(
        "AND source = 'local'
                AND file_path >= ?
                AND file_path <  ?
                AND (? = 1 OR instr(substr(file_path, ?), ?) = 0)",
        "ORDER BY file_path",
    );
    for (recursive, expected) in [(0_i64, 1_usize), (1, 5)] {
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
            .bind(Some(1_i64))
            .bind(Some(1_i64))
            .bind(Some(1_i64))
            .bind(Option::<String>::None)
            .bind(Option::<String>::None)
            .bind(&low)
            .bind(&high)
            .bind(recursive)
            .bind(low.chars().count() as i64 + 1)
            .bind(&sep)
            .fetch_all(&pool)
            .await
            .unwrap();
        // Direct: only `top.flac`. Recursive: everything under the
        // root, which is what "play this folder" needs.
        assert_eq!(rows.len(), expected, "recursive = {recursive}");
    }
}

#[tokio::test]
async fn folder_ids_come_back_in_path_order() {
    let pool = pool().await;
    seed_folders(&pool).await;
    let (low, high) = folder_prefix_range(&p(&["", "m", "Rock"]));
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM track
              WHERE (? IS NULL OR library_id = ?)
                AND is_available = 1
                AND file_path >= ?
                AND file_path <  ?
              ORDER BY file_path COLLATE NOCASE",
    )
    .bind(Some(1_i64))
    .bind(Some(1_i64))
    .bind(&low)
    .bind(&high)
    .fetch_all(&pool)
    .await
    .unwrap();
    // Album A, then Album B's two discs, then the stray top-level
    // file: the order the files sit in on disk.
    assert_eq!(ids, vec![11, 12, 13, 14, 10]);
}

// ── Pinyin search (#579) ────────────────────────────────────────

/// Every FTS row that matches `query`, through the index the search
/// command's indexed route uses.
async fn fts_hits(pool: &SqlitePool, query: &str) -> Vec<i64> {
    sqlx::query_scalar("SELECT rowid FROM track_fts WHERE track_fts MATCH ? ORDER BY rowid")
        .bind(query)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// A library that predates the pinyin column: rows in place, blobs
/// still NULL, exactly what the migration leaves behind.
async fn seed_chinese(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        "INSERT INTO artist (id, name, canonical_name) VALUES (1, '周杰伦', '周杰伦')",
        "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (1, '范特西', '范特西', 1, 0)",
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, primary_artist, duration_ms, added_at,
                                is_available, hlc_wall, hlc_logical,
                                rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/m/1.flac', 'h1', 1, 0, '中国人民解放军', 1, 1, 1000, 0, 1,
                     0, 0, 0, 0)",
    ] {
        sqlx::raw_sql(statement).execute(pool).await.unwrap();
    }
}

#[tokio::test]
async fn a_backfilled_track_becomes_searchable_by_pinyin() {
    let pool = pool().await;
    seed_chinese(&pool).await;

    // Before the backfill the blob is NULL, so the index holds
    // nothing to match -- the state every existing library is in the
    // moment the migration lands.
    assert!(fts_hits(&pool, "\"zhongguoren\"").await.is_empty());

    // What the backfill does, and the only thing it does.
    sqlx::query("UPDATE track SET pinyin = ? WHERE id = 1")
        .bind("zhongguoren zgr")
        .execute(&pool)
        .await
        .unwrap();

    // The trigger has to have carried it into the index: without
    // `pinyin` in its watched columns, a backfilled row would stay
    // unsearchable until something else touched its title.
    assert_eq!(fts_hits(&pool, "\"zhongguoren\"").await, vec![1]);
    // And the syllables are reachable from the middle, because the
    // tokenizer is trigram.
    assert_eq!(fts_hits(&pool, "\"guoren\"").await, vec![1]);
}

#[tokio::test]
async fn initials_reach_the_track_too() {
    let pool = pool().await;
    seed_chinese(&pool).await;
    sqlx::query("UPDATE track SET pinyin = ? WHERE id = 1")
        .bind("zhongguoren zgr")
        .execute(&pool)
        .await
        .unwrap();
    // The form the request actually asked for: three initials, which
    // is also the shortest thing trigram can answer.
    assert_eq!(fts_hits(&pool, "\"zgr\"").await, vec![1]);
}

#[tokio::test]
async fn an_artist_or_album_blob_reaches_its_tracks() {
    let pool = pool().await;
    seed_chinese(&pool).await;

    // The backfill walks three tables; these are the other two, and
    // each has its own trigger to carry the blob onto every track
    // that points at it.
    sqlx::query("UPDATE artist SET pinyin = ? WHERE id = 1")
        .bind("zhoujielun zjl")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(fts_hits(&pool, "\"zhoujielun\"").await, vec![1]);

    sqlx::query("UPDATE album SET pinyin = ? WHERE id = 1")
        .bind("fantexi ftx")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(fts_hits(&pool, "\"fantexi\"").await, vec![1]);
    // The album write must not have cost the artist's blob: the
    // trigger rebuilds the whole concatenation from the row, so all
    // three sources survive each other.
    assert_eq!(fts_hits(&pool, "\"zhoujielun\"").await, vec![1]);
}

#[tokio::test]
async fn a_scanned_track_carries_its_pinyin_without_a_backfill() {
    let pool = pool().await;
    seed_chinese(&pool).await;
    // What the scanner writes: the blob is part of the INSERT, so
    // the insert trigger indexes it immediately.
    sqlx::query(
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, primary_artist, duration_ms, added_at,
                                is_available, pinyin, hlc_wall, hlc_logical,
                                rating_hlc_wall, rating_hlc_logical)
             VALUES (2, 1, '/m/2.flac', 'h2', 1, 0, '稻香', 1, 1, 1000, 0, 1, ?, 0, 0, 0, 0)",
    )
    .bind("daoxiang dx")
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(fts_hits(&pool, "\"daoxiang\"").await, vec![2]);
}

#[tokio::test]
async fn a_renamed_title_does_not_leave_a_stale_blob() {
    let pool = pool().await;
    seed_chinese(&pool).await;
    sqlx::query("UPDATE track SET pinyin = ? WHERE id = 1")
        .bind("zhongguoren zgr")
        .execute(&pool)
        .await
        .unwrap();

    // The tag editor rewrites both columns together. The old
    // romanisation must not answer any more -- the backfill only
    // looks at NULLs, so a blob left behind here would stay wrong
    // for the life of the library.
    sqlx::query("UPDATE track SET title = ?, pinyin = ? WHERE id = 1")
        .bind("稻香")
        .bind("daoxiang dx")
        .execute(&pool)
        .await
        .unwrap();
    assert!(fts_hits(&pool, "\"zhongguoren\"").await.is_empty());
    assert_eq!(fts_hits(&pool, "\"daoxiang\"").await, vec![1]);
}

#[tokio::test]
async fn the_text_route_still_works_beside_the_pinyin_one() {
    let pool = pool().await;
    seed_chinese(&pool).await;
    sqlx::query("UPDATE track SET pinyin = ? WHERE id = 1")
        .bind("zhongguoren zgr")
        .execute(&pool)
        .await
        .unwrap();
    // The defect the trigram rebuild fixed, re-checked with a fourth
    // column in the table: characters from the MIDDLE of the title.
    // Three of them, because that is trigram's floor -- two would
    // match nothing here and take the LIKE route in the real search.
    assert_eq!(fts_hits(&pool, "\"民解放\"").await, vec![1]);
}

async fn albums(
    pool: &SqlitePool,
    library_id: Option<i64>,
    source: Option<&str>,
    order: &str,
) -> Vec<(String, String)> {
    sqlx::query(sqlx::AssertSqlSafe(library_albums_sql(order)))
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .bind(source)
        .bind(source)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get("source"), row.get("title")))
        .collect()
}

async fn artists(
    pool: &SqlitePool,
    library_id: Option<i64>,
    source: Option<&str>,
    order: &str,
) -> Vec<(String, String, i64, i64)> {
    sqlx::query(sqlx::AssertSqlSafe(library_artists_sql(order)))
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .bind(source)
        .bind(source)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.get("source"),
                row.get("name"),
                row.get("track_count"),
                row.get("album_count"),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_album_listing_sorts_both_halves_against_each_other() {
    let pool = pool().await;
    seed(&pool).await;

    let by_artist = albums(&pool, None, None, library_album_order_clause(None, None)).await;
    // "aphex twin" before "bjork": the remote half's normalised key sorts
    // against the local half's canonical one, which is the whole point.
    assert_eq!(
        by_artist,
        vec![
            ("remote".into(), "Drukqs".into()),
            ("local".into(), "Vespertine".into()),
        ]
    );

    let by_title_desc = albums(
        &pool,
        None,
        None,
        library_album_order_clause(Some("title"), Some("desc")),
    )
    .await;
    assert_eq!(
        by_title_desc.first().map(|row| row.1.as_str()),
        Some("Vespertine")
    );
}

#[tokio::test]
async fn the_source_filter_returns_one_half_only() {
    let pool = pool().await;
    seed(&pool).await;
    let order = library_album_order_clause(None, None);

    assert_eq!(
        albums(&pool, None, Some("local"), order).await,
        vec![("local".to_string(), "Vespertine".to_string())]
    );
    assert_eq!(
        albums(&pool, None, Some("remote"), order).await,
        vec![("remote".to_string(), "Drukqs".to_string())]
    );
}

/// The picker chooses among *local* libraries, and a server album belongs
/// to none of them.
#[tokio::test]
async fn a_local_library_filter_excludes_the_remote_half() {
    let pool = pool().await;
    seed(&pool).await;
    let order = library_album_order_clause(None, None);

    assert_eq!(
        albums(&pool, Some(1), None, order).await,
        vec![("local".to_string(), "Vespertine".to_string())]
    );
    // A library that holds nothing leaves nothing, rather than falling
    // back to the remote half.
    assert!(albums(&pool, Some(99), None, order).await.is_empty());
}

async fn tracks(
    pool: &SqlitePool,
    library_id: Option<i64>,
    source: Option<&str>,
    order: &str,
) -> Vec<(String, String, Option<i64>)> {
    sqlx::query(sqlx::AssertSqlSafe(library_tracks_sql(order)))
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .bind(source)
        .bind(source)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get("source"), row.get("title"), row.get("rating")))
        .collect()
}

#[tokio::test]
async fn the_track_listing_sorts_both_halves_against_each_other() {
    let pool = pool().await;
    seed(&pool).await;

    let rows = tracks(&pool, None, None, &library_track_order_clause(None, None)).await;
    // "aphex twin" before "bjork", from the normalised keys on both sides.
    assert_eq!(
        rows.iter().map(|row| row.1.as_str()).collect::<Vec<_>>(),
        vec!["R1", "R2", "T1"]
    );
    // A server track has no rating, and that is not the same as unrated.
    assert_eq!(rows[0].2, None);
}

/// A track cached for a playlist is not part of the catalogue, and the
/// library must not list it — it would appear with no album and no way to
/// reach it.
#[tokio::test]
async fn the_track_listing_shows_only_the_mirrored_catalogue() {
    let pool = pool().await;
    seed(&pool).await;

    let titles: Vec<String> = tracks(&pool, None, None, &library_track_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert!(!titles.iter().any(|title| title == "Not in the catalogue"));
}

/// A confirmed link proves the two rows are the same bytes, so the list
/// must show one of them — the local one, which is the playable half.
#[tokio::test]
async fn a_linked_server_track_stops_being_a_second_row() {
    let pool = pool().await;
    seed(&pool).await;
    sqlx::raw_sql(
        "INSERT INTO remote_track_link
                 (local_track_id, remote_track_id, method, verified_full_hash,
                  status, playback_preference, confirmed_at, verified_at)
             VALUES (1, 't-1', 'exact_full_hash',
                     '0000000000000000000000000000000000000000000000000000000000000000',
                     'confirmed', 'local_first', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let titles: Vec<String> = tracks(&pool, None, None, &library_track_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(titles, vec!["R2".to_string(), "T1".to_string()]);
}

/// Two narrowings on that predicate, and dropping either one loses a
/// track rather than a duplicate.
#[tokio::test]
async fn a_link_hides_nothing_it_cannot_prove() {
    let pool = pool().await;
    seed(&pool).await;
    sqlx::raw_sql(
        "INSERT INTO remote_track_link
                 (local_track_id, remote_track_id, method, verified_full_hash,
                  status, playback_preference, confirmed_at, verified_at)
             VALUES (1, 't-1', 'exact_full_hash',
                     '0000000000000000000000000000000000000000000000000000000000000000',
                     'stale', 'local_first', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // A stale link is a guess. Hiding on a guess loses the track.
    let stale: Vec<String> = tracks(&pool, None, None, &library_track_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert!(stale.iter().any(|title| title == "R1"), "{stale:?}");

    // Confirmed, but the local file has gone: the local half already
    // filtered itself out, so hiding the remote half too would remove the
    // recording from the library altogether — and the server can play it.
    sqlx::raw_sql("UPDATE remote_track_link SET status = 'confirmed'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("UPDATE track SET is_available = 0 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let gone: Vec<String> = tracks(&pool, None, None, &library_track_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(gone, vec!["R1".to_string(), "R2".to_string()]);
}

/// Mark a local track as examined — RFC-006's completeness frontier. Every
/// pairing test needs this, because an unexamined track can never pair.
async fn examine(pool: &SqlitePool, track_id: i64) {
    sqlx::query(
        "INSERT INTO local_full_hash (track_id, full_hash, file_size, file_modified, computed_at)
             SELECT t.id, ?, t.file_size, t.file_modified, 0 FROM track t WHERE t.id = ?",
    )
    .bind("a".repeat(64))
    .bind(track_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn link(pool: &SqlitePool, local: i64, remote: &str) {
    sqlx::query(
        "INSERT INTO remote_track_link
                 (local_track_id, remote_track_id, method, verified_full_hash,
                  status, playback_preference, confirmed_at, verified_at)
             VALUES (?, ?, 'exact_full_hash', ?, 'confirmed', 'local_first', 0, 0)",
    )
    .bind(local)
    .bind(remote)
    .bind("0".repeat(64))
    .execute(pool)
    .await
    .unwrap();
}

/// A local album and a server album whose track sets are in complete
/// bijection are one release, and the listing shows it once.
#[tokio::test]
async fn an_album_pairs_only_on_a_complete_bijection() {
    let pool = pool().await;
    seed(&pool).await;
    // The fixture's local album holds one track; give the server album one
    // too, so a bijection is possible at all.
    sqlx::raw_sql("DELETE FROM remote_track WHERE remote_id = 't-2'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("UPDATE remote_album SET song_count = 1, mirrored_at = 1")
        .execute(&pool)
        .await
        .unwrap();

    // Linked but not yet examined: still two entries, because "no link" on
    // an unexamined track cannot be read as "different bytes".
    link(&pool, 1, "t-1").await;
    let titles: Vec<String> = albums(&pool, None, None, library_album_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(titles, vec!["Drukqs".to_string(), "Vespertine".to_string()]);

    examine(&pool, 1).await;
    let titles: Vec<String> = albums(&pool, None, None, library_album_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(
        titles,
        vec!["Vespertine".to_string()],
        "one release, one entry"
    );
}

/// The case that killed the RFC's first draft: two releases sharing
/// recordings are not the same release, however unanimous the links.
#[tokio::test]
async fn sharing_recordings_is_not_being_the_same_album() {
    let pool = pool().await;
    seed(&pool).await;
    sqlx::raw_sql("UPDATE remote_album SET mirrored_at = 1")
        .execute(&pool)
        .await
        .unwrap();
    // The server album keeps two tracks; the local one has a single track
    // linked to one of them. A "unanimity plus links" rule would pair them.
    link(&pool, 1, "t-1").await;
    examine(&pool, 1).await;

    let titles: Vec<String> = albums(&pool, None, None, library_album_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(
        titles,
        vec!["Drukqs".to_string(), "Vespertine".to_string()],
        "an unmatched server track must keep the albums apart"
    );
}

/// A bijection over two empty sets holds vacuously. It must not pair.
#[tokio::test]
async fn an_album_with_no_tracks_never_pairs() {
    let pool = pool().await;
    seed(&pool).await;
    sqlx::raw_sql("DELETE FROM remote_track")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("UPDATE remote_album SET song_count = 0, mirrored_at = 1")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("DELETE FROM track")
        .execute(&pool)
        .await
        .unwrap();

    let sources: Vec<String> = albums(&pool, None, None, library_album_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.0)
        .collect();
    assert_eq!(
        sources,
        vec!["remote".to_string()],
        "the empty pair must not collapse"
    );
}

/// A server album still being walked is a prefix, not a set, so nothing
/// pairs against it however complete the links look.
#[tokio::test]
async fn an_unwalked_album_never_pairs() {
    let pool = pool().await;
    seed(&pool).await;
    sqlx::raw_sql("DELETE FROM remote_track WHERE remote_id = 't-2'")
        .execute(&pool)
        .await
        .unwrap();
    link(&pool, 1, "t-1").await;
    examine(&pool, 1).await;
    // `mirrored_at` left NULL: the walk has not finished.
    let titles: Vec<String> = albums(&pool, None, None, library_album_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(titles.len(), 2, "{titles:?}");
}

/// Artists pair on a sample, not on a set — but the sample has to be more
/// than one link, and unanimous.
#[tokio::test]
async fn an_artist_pairs_on_two_unanimous_links() {
    let pool = pool().await;
    seed(&pool).await;
    // A second local track by the same artist, so two links are possible.
    sqlx::raw_sql(
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available,
                                primary_artist, hlc_wall, hlc_logical,
                                rating_hlc_wall, rating_hlc_logical)
             VALUES (2, 1, '/m/2.flac', 'h2', 2, 0, 'T2', 1, 300000, 500, 1, 1, 0, 0, 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    examine(&pool, 1).await;
    examine(&pool, 2).await;

    link(&pool, 1, "t-1").await;
    let names: Vec<String> = artists(&pool, None, None, library_artist_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(names.len(), 2, "one link is a coincidence, not evidence");

    link(&pool, 2, "t-2").await;
    let names: Vec<String> = artists(&pool, None, None, library_artist_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(names, vec!["Björk".to_string()], "two unanimous links pair");
}

/// Unanimity is what disposes of Various Artists, with no `is_compilation`
/// special case: a compilation's links point at several server artists, so
/// the disagreeing one ends the question.
#[tokio::test]
async fn a_disagreeing_link_keeps_two_artists_apart() {
    let pool = pool().await;
    seed(&pool).await;
    sqlx::raw_sql(
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available,
                                primary_artist, hlc_wall, hlc_logical,
                                rating_hlc_wall, rating_hlc_logical)
             VALUES (2, 1, '/m/2.flac', 'h2', 2, 0, 'T2', 1, 300000, 500, 1, 1, 0, 0, 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // A third server track credited to somebody else entirely.
    sqlx::raw_sql(
        "INSERT INTO remote_artist (remote_id, name, sort_key, mirrored_at)
             VALUES ('ar-2', 'Someone Else', 'someone else', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        "INSERT INTO remote_track (remote_id, title, artist_id, duration_ms, cached_at,
                                       in_catalogue)
             VALUES ('t-9', 'R9', 'ar-2', 0, 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    examine(&pool, 1).await;
    examine(&pool, 2).await;

    link(&pool, 1, "t-1").await;
    link(&pool, 2, "t-2").await;
    // Two unanimous links would pair — but this local artist also owns a
    // track linked to a different server artist.
    sqlx::raw_sql(
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, duration_ms, added_at, is_available,
                                primary_artist, hlc_wall, hlc_logical,
                                rating_hlc_wall, rating_hlc_logical)
             VALUES (3, 1, '/m/3.flac', 'h3', 3, 0, 'T3', 300000, 500, 1, 1, 0, 0, 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    examine(&pool, 3).await;
    link(&pool, 3, "t-9").await;

    let names: Vec<String> = artists(&pool, None, None, library_artist_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(
        names.len(),
        3,
        "a disagreeing link ends the question: {names:?}"
    );
}

/// An artist credited on a track but never its primary is not paired, and
/// appears twice.
///
/// The mirror holds one artist per server track, so `primary_artist` is
/// the only comparable local column; a guest credit would otherwise read
/// as a disagreement about an artist the server never mentioned. A visible
/// duplicate is the failure this design chooses.
#[tokio::test]
async fn a_guest_credit_alone_never_pairs_an_artist() {
    let pool = pool().await;
    seed(&pool).await;
    // A second local artist, credited on both tracks but primary on
    // neither — exactly what a featuring credit looks like.
    sqlx::raw_sql("INSERT INTO artist (id, name, canonical_name) VALUES (2, 'Guest', 'guest')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available,
                                primary_artist, hlc_wall, hlc_logical,
                                rating_hlc_wall, rating_hlc_logical)
             VALUES (2, 1, '/m/2.flac', 'h2', 2, 0, 'T2', 1, 300000, 500, 1, 1, 0, 0, 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        "INSERT INTO track_artist (track_id, artist_id, position)
             VALUES (1, 2, 1), (2, 1, 0), (2, 2, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    examine(&pool, 1).await;
    examine(&pool, 2).await;
    link(&pool, 1, "t-1").await;
    link(&pool, 2, "t-2").await;

    let names: Vec<String> = artists(&pool, None, None, library_artist_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    // Björk is primary on both linked tracks and pairs; Guest is credited
    // on both and pairs with nothing, so it stays listed on its own.
    assert!(names.contains(&"Guest".to_string()), "{names:?}");
    assert_eq!(
        names.iter().filter(|name| *name == "Björk").count(),
        1,
        "the primary artist still pairs: {names:?}"
    );
}

/// Rating is local-only, so the remote half has none. Sorting by it must
/// not read a missing rating as the worst one.
#[tokio::test]
async fn sorting_by_rating_puts_the_unratable_last_either_way() {
    let pool = pool().await;
    seed(&pool).await;
    sqlx::raw_sql("UPDATE track SET rating = 200 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();

    for direction in ["asc", "desc"] {
        let rows = tracks(
            &pool,
            None,
            None,
            &library_track_order_clause(Some("rating"), Some(direction)),
        )
        .await;
        assert_eq!(rows[0].1, "T1", "{direction}: the rated track leads");
        assert!(rows[1..].iter().all(|row| row.2.is_none()));
    }
}

/// The sort dropdown and the persisted preference both carry the column
/// name. Matching on anything else sends the sort to the fallback clause
/// and does nothing visible — which is the quietest way for a sort to be
/// broken.
#[tokio::test]
async fn sorting_by_duration_uses_the_key_the_dropdown_sends() {
    let pool = pool().await;
    seed(&pool).await;
    sqlx::raw_sql("UPDATE remote_track SET duration_ms = 999999 WHERE remote_id = 't-1'")
        .execute(&pool)
        .await
        .unwrap();

    let longest = tracks(
        &pool,
        None,
        None,
        &library_track_order_clause(Some("duration_ms"), Some("desc")),
    )
    .await;
    assert_eq!(longest.first().map(|row| row.1.as_str()), Some("R1"));

    let shortest = tracks(
        &pool,
        None,
        None,
        &library_track_order_clause(Some("duration_ms"), Some("asc")),
    )
    .await;
    assert_eq!(shortest.last().map(|row| row.1.as_str()), Some("R1"));
}

#[tokio::test]
async fn the_track_filters_behave_like_the_album_ones() {
    let pool = pool().await;
    seed(&pool).await;
    let order = library_track_order_clause(None, None);

    let local = tracks(&pool, None, Some("local"), &order).await;
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].0, "local");

    let remote = tracks(&pool, None, Some("remote"), &order).await;
    assert_eq!(remote.len(), 2);
    assert!(remote.iter().all(|row| row.0 == "remote"));

    let scoped = tracks(&pool, Some(1), None, &order).await;
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].0, "local");
}

#[tokio::test]
async fn the_artist_listing_sorts_and_derives_its_counts() {
    let pool = pool().await;
    seed(&pool).await;

    let rows = artists(&pool, None, None, library_artist_order_clause(None, None)).await;
    assert_eq!(
        rows,
        vec![
            // Two mirrored tracks and one mirrored album, counted from the
            // rows the mirror holds rather than from a stored total.
            ("remote".to_string(), "Aphex Twin".to_string(), 2, 1),
            ("local".to_string(), "Björk".to_string(), 1, 1),
        ]
    );

    let by_tracks = artists(
        &pool,
        None,
        None,
        library_artist_order_clause(Some("tracks_count"), Some("desc")),
    )
    .await;
    assert_eq!(
        by_tracks.first().map(|row| row.1.as_str()),
        Some("Aphex Twin")
    );
}

/// The scanner's canonical forms keep their accents, so an ASCII-only
/// collation put "Émilie Simon" after "Zazie", past the whole alphabet,
/// while the A-Z rail files her under E.
#[tokio::test]
async fn an_accented_initial_sorts_with_its_letter() {
    let pool = pool().await;
    seed(&pool).await;
    for (id, name, canonical) in [(2, "Zazie", "zazie"), (3, "Émilie Simon", "émilie simon")] {
        sqlx::query("INSERT INTO artist (id, name, canonical_name) VALUES (?, ?, ?)")
            .bind(id)
            .bind(name)
            .bind(canonical)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
                 VALUES (?, ?, ?, ?, 2000, 0)",
        )
        .bind(id)
        .bind(name)
        .bind(canonical)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, album_id, primary_artist,
                                    duration_ms, added_at, is_available, hlc_wall,
                                    hlc_logical, rating_hlc_wall, rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, 'T', ?, ?, 1000, 500, 1, 0, 0, 0, 0)",
        )
        .bind(id)
        .bind(format!("/m/{id}.flac"))
        .bind(format!("h{id}"))
        .bind(id)
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

    let names: Vec<String> = artists(&pool, None, None, library_artist_order_clause(None, None))
        .await
        .into_iter()
        .map(|row| row.1)
        .collect();
    assert_eq!(names, ["Aphex Twin", "Björk", "Émilie Simon", "Zazie"]);

    let titles: Vec<String> = albums(
        &pool,
        None,
        None,
        library_album_order_clause(Some("title"), Some("asc")),
    )
    .await
    .into_iter()
    .map(|row| row.1)
    .collect();
    assert_eq!(titles, ["Drukqs", "Émilie Simon", "Vespertine", "Zazie"]);
}

#[tokio::test]
async fn the_artist_filters_behave_like_the_album_ones() {
    let pool = pool().await;
    seed(&pool).await;
    let order = library_artist_order_clause(None, None);

    let local = artists(&pool, None, Some("local"), order).await;
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].0, "local");

    let remote = artists(&pool, None, Some("remote"), order).await;
    assert_eq!(remote.len(), 1);
    assert_eq!(remote[0].0, "remote");

    // A local library filter drops the remote half here too.
    let scoped = artists(&pool, Some(1), None, order).await;
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].0, "local");
}
