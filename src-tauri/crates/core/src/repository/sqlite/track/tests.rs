use super::*;
use crate::search::plan_search;
use std::str::FromStr;

/// Enough of the profile schema for `SELECT_TRACK_ROW` to resolve,
/// plus the FTS index the search actually reads.
///
/// The `track_fts` definition here MUST stay identical to
/// `migrations/profile/20260912210000_pinyin_search.sql` (which
/// rebuilt it with a fourth column). The
/// migrations live in the app crate, out of reach from
/// `waveflow-core` (same constraint the scanner fixtures note), so
/// this is a copy — if the tokenizer changes there, change it here.
async fn fixture_pool() -> SqlitePool {
    let options = super::super::collation::register(
        sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:").unwrap(),
    );
    let pool = SqlitePool::connect_with(options).await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys = ON;
             CREATE TABLE artwork (
                 id INTEGER PRIMARY KEY,
                 hash TEXT NOT NULL,
                 format TEXT NOT NULL
             );
             CREATE TABLE artist (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 canonical_name TEXT,
                 pinyin TEXT
             );
             CREATE TABLE album (
                 id INTEGER PRIMARY KEY,
                 title TEXT NOT NULL,
                 canonical_title TEXT,
                 artist_id INTEGER REFERENCES artist(id),
                 artwork_id INTEGER REFERENCES artwork(id),
                 pinyin TEXT
             );
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 library_id INTEGER NOT NULL,
                 title TEXT NOT NULL,
                 album_id INTEGER REFERENCES album(id),
                 primary_artist INTEGER REFERENCES artist(id),
                 duration_ms INTEGER NOT NULL DEFAULT 0,
                 track_number INTEGER, disc_number INTEGER, year INTEGER,
                 bitrate INTEGER, sample_rate INTEGER, channels INTEGER,
                 bit_depth INTEGER, codec TEXT, musical_key TEXT,
                 file_path TEXT NOT NULL, file_size INTEGER NOT NULL DEFAULT 0,
                 added_at INTEGER NOT NULL DEFAULT 0,
                 rating INTEGER,
                 pinyin TEXT,
                 is_available INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE track_artist (
                 track_id INTEGER NOT NULL,
                 artist_id INTEGER NOT NULL,
                 position INTEGER NOT NULL
             );
             -- What `effective_musical_key_sql!` reads.
             CREATE TABLE track_analysis (
                 track_id INTEGER PRIMARY KEY,
                 musical_key TEXT
             );
             CREATE VIRTUAL TABLE track_fts USING fts5(
                 title, album_title, artist_name, pinyin,
                 tokenize='trigram remove_diacritics 1'
             );",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// One Chinese track, one Latin track, one unavailable track.
async fn seed(pool: &SqlitePool) {
    sqlx::raw_sql(
        "INSERT INTO artist (id, name, pinyin)
             VALUES (1, '周杰伦', 'zhoujielun zjl'), (2, 'U2', '');
             INSERT INTO album (id, title, artist_id, pinyin)
             VALUES (1, '叶惠美', 1, 'yehuimei yhm'), (2, 'War', 2, '');
             INSERT INTO track (id, library_id, title, album_id, primary_artist, file_path,
                                pinyin)
             VALUES (1, 1, '中国人民解放军进行曲', 1, 1, '/a.flac',
                     'zhongguorenminjiefangjunjinxingqu zgrmjfjjxq'),
                    (2, 1, 'Sunday Bloody Sunday', 2, 2, '/b.flac', '');
             INSERT INTO track (id, library_id, title, album_id, primary_artist,
                                file_path, pinyin, is_available)
             VALUES (3, 1, '中国人民解放军进行曲 (live)', 1, 1, '/c.flac',
                     'zhongguorenminjiefangjunjinxingqu zgrmjfjjxq', 0);
             INSERT INTO track_artist (track_id, artist_id, position)
             VALUES (1, 1, 0), (2, 2, 0), (3, 1, 0);
             INSERT INTO track_fts (rowid, title, album_title, artist_name, pinyin)
             SELECT t.id, t.title,
                    COALESCE(al.title, ''), COALESCE(ar.name, ''),
                    COALESCE(t.pinyin, '') || ' ' || COALESCE(al.pinyin, '') || ' '
                        || COALESCE(ar.pinyin, '')
               FROM track t
               LEFT JOIN album  al ON al.id = t.album_id
               LEFT JOIN artist ar ON ar.id = t.primary_artist;",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// The key a track shows is its tag's, and the analysis's only when
/// the tag is missing or blank.
#[tokio::test]
async fn a_tagged_key_wins_over_the_analysed_one() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    sqlx::raw_sql(
        "UPDATE track SET musical_key = '8A' WHERE id = 1;
             UPDATE track SET musical_key = '  ' WHERE id = 2;
             INSERT INTO track_analysis (track_id, musical_key)
             VALUES (1, 'C'), (2, 'Ebm');",
    )
    .execute(&pool)
    .await
    .unwrap();
    let repo = SqliteTrackRepository::new(pool.clone());
    let key = |row: Option<TrackRow>| row.unwrap().musical_key;
    assert_eq!(key(repo.get(1).await.unwrap()).as_deref(), Some("8A"));
    assert_eq!(key(repo.get(2).await.unwrap()).as_deref(), Some("Ebm"));
    sqlx::raw_sql("DELETE FROM track_analysis WHERE track_id = 2;")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(key(repo.get(2).await.unwrap()), None);
}

async fn listed_for_artist(pool: &SqlitePool, artist_id: i64) -> Vec<i64> {
    let repo = SqliteTrackRepository::new(pool.clone());
    let filter = TrackListFilter {
        library_id: None,
        artist_id: Some(artist_id),
    };
    let mut ids: Vec<i64> = repo
        .list(filter, TrackSort::default())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    ids.sort();
    ids
}

/// A comma inside an artist's name, a featured credit, and an album
/// artist credited on no track: all three found by row id.
#[tokio::test]
async fn an_artist_filter_matches_credits_and_album_artists_by_id() {
    let pool = fixture_pool().await;
    sqlx::raw_sql(
        "INSERT INTO artist (id, name) VALUES
                 (1, 'Earth, Wind & Fire'), (2, 'Guest'), (3, 'Various Artists'), (4, 'Other');
             INSERT INTO album (id, title, artist_id) VALUES (1, 'Hits', 1), (2, 'Comp', 3);
             INSERT INTO track (id, library_id, title, album_id, primary_artist, file_path)
             VALUES (1, 1, 'September', 1, 1, '/1'),
                    (2, 1, 'Duet', 1, 1, '/2'),
                    (3, 1, 'On a compilation', 2, 4, '/3'),
                    (4, 1, 'Unrelated', NULL, 4, '/4');
             INSERT INTO track (id, library_id, title, album_id, primary_artist, file_path,
                                is_available)
             VALUES (5, 1, 'Gone', 1, 1, '/5', 0);
             INSERT INTO track_artist (track_id, artist_id, position) VALUES
                 (1, 1, 0), (2, 1, 0), (2, 2, 1), (3, 4, 0), (4, 4, 0), (5, 1, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(listed_for_artist(&pool, 1).await, vec![1, 2]);
    assert_eq!(
        listed_for_artist(&pool, 2).await,
        vec![2],
        "a featured credit"
    );
    assert_eq!(
        listed_for_artist(&pool, 3).await,
        vec![3],
        "album artist only"
    );
    assert_eq!(listed_for_artist(&pool, 4).await, vec![3, 4]);
}

async fn ids_for(pool: &SqlitePool, query: &str) -> Vec<i64> {
    let plan = plan_search(query).expect("query should produce a plan");
    let repo = SqliteTrackRepository::new(pool.clone());
    repo.search(&plan, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect()
}

/// The other half of #579: typing the romanisation, on a Latin
/// keyboard, without switching input method.
#[tokio::test]
async fn a_pinyin_query_finds_a_chinese_title() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    // Syllables, from the start and from the middle -- the blob is
    // indexed with the same trigram tokenizer as the text.
    assert_eq!(ids_for(&pool, "zhongguo").await, vec![1]);
    assert_eq!(ids_for(&pool, "jiefang").await, vec![1]);
    // Initials, which is what someone types when they know the
    // title but not how to spell it out.
    assert_eq!(ids_for(&pool, "zgrmjfjjxq").await, vec![1]);
    // And the Latin track is not dragged in by either.
    assert!(!ids_for(&pool, "zhongguo").await.contains(&2));
}

/// A two-character term is below the trigram floor, so it takes the
/// scanned route -- which is exactly where a short pinyin initialism
/// lands, and why the `LIKE` clause had to learn the blob columns.
#[tokio::test]
async fn short_pinyin_initials_take_the_like_route() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    assert_eq!(ids_for(&pool, "zg").await, vec![1]);
}

/// The blob of an album or an artist reaches their tracks, because
/// the index carries all three concatenated.
#[tokio::test]
async fn an_album_or_artist_romanisation_reaches_the_track() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    assert_eq!(ids_for(&pool, "zhoujielun").await, vec![1]);
    assert_eq!(ids_for(&pool, "yehuimei").await, vec![1]);
}

/// The whole point of #579: characters from the MIDDLE of a Chinese
/// title find it. Before trigram this returned nothing.
#[tokio::test]
async fn a_chinese_title_is_found_by_its_middle() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    assert_eq!(ids_for(&pool, "人民解").await, vec![1]);
    // ...and by its start, which already worked and must not break.
    assert_eq!(ids_for(&pool, "中国人").await, vec![1]);
}

/// The fallback route is not a formality: two Han characters is the
/// common shape of a Chinese word, and `MATCH` cannot serve it.
#[tokio::test]
async fn a_two_character_query_takes_the_like_route_and_still_matches() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    assert_eq!(ids_for(&pool, "人民").await, vec![1]);
    // Short Latin names go the same way rather than regressing.
    assert_eq!(ids_for(&pool, "u2").await, vec![2]);
}

/// Album and artist are indexed alongside the title on the MATCH
/// route, and searched alongside it on the LIKE one.
#[tokio::test]
async fn both_routes_reach_album_and_artist() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    assert_eq!(ids_for(&pool, "叶惠美").await, vec![1]); // album, indexed
    assert_eq!(ids_for(&pool, "惠美").await, vec![1]); // album, scanned
    assert_eq!(ids_for(&pool, "周杰伦").await, vec![1]); // artist, indexed
                                                         // The artist column on the scanned route. `u2` covers it too,
                                                         // but only incidentally — this is the assertion that fails if
                                                         // `ar.name` ever drops out of `LIKE_TERM_CLAUSE`.
    assert_eq!(ids_for(&pool, "周杰").await, vec![1]);
}

/// A known limitation, pinned so it stays deliberate: SQLite's
/// `LIKE` folds case for ASCII only, so on the scanned route a
/// lowercase accented term does not find its capitalised form.
///
/// It affects terms of one or two characters and nothing else — from
/// three characters up the trigram index answers, and it was built
/// with `remove_diacritics 1`, which folds both case and accents.
/// Before #579 the old tokenizer folded at every length, so this is
/// the one place the change gives something up.
///
/// Closing it needs a folded column on `track` (the shape `album`
/// and `artist` already have in `canonical_title` / `canonical_name`),
/// which is a migration and a scanner change rather than a tweak
/// here. Tracked on #579.
#[tokio::test]
async fn a_short_accented_term_does_not_fold_case_on_the_scanned_route() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    sqlx::raw_sql(
        "INSERT INTO track (id, library_id, title, file_path)
             VALUES (4, 1, 'Été indien', '/d.flac');
             INSERT INTO track_fts (rowid, title, album_title, artist_name)
             VALUES (4, 'Été indien', '', '');",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Three characters: the index answers, and folds.
    assert_eq!(ids_for(&pool, "ete").await, vec![4]);
    assert_eq!(ids_for(&pool, "ÉTÉ").await, vec![4]);
    // Two characters: a scan, and `É` no longer matches `é`.
    assert_eq!(ids_for(&pool, "Ét").await, vec![4]);
    assert!(ids_for(&pool, "ét").await.is_empty());
}

/// Multiple terms are an AND on both routes, not an OR.
#[tokio::test]
async fn terms_are_combined_with_and() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    assert_eq!(ids_for(&pool, "sunday bloody").await, vec![2]);
    assert!(ids_for(&pool, "sunday 人民解").await.is_empty());
}

/// A track whose file went missing must not surface on either route
/// — the seed has one, indexed, deliberately.
#[tokio::test]
async fn unavailable_tracks_stay_out_of_both_routes() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    assert_eq!(ids_for(&pool, "解放军进").await, vec![1]);
    assert_eq!(ids_for(&pool, "人民").await, vec![1]);
}

/// An unescaped `%` on the LIKE route would return the whole
/// library. It reaches SQLite as a literal instead.
#[tokio::test]
async fn a_percent_sign_is_not_a_wildcard() {
    let pool = fixture_pool().await;
    seed(&pool).await;
    assert!(ids_for(&pool, "%").await.is_empty());
    assert!(ids_for(&pool, "_").await.is_empty());
}
