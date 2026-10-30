use super::*;
use crate::enrichment::{AlbumFacts, Credit};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// The real profile schema, foreign keys on: a constraint the migration
/// declares is one these tests meet.
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
    sqlx::query("INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artist (id, name, canonical_name) VALUES (1, 'Nirvana', 'nirvana')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO album (id, title, canonical_title, artist_id) VALUES (1, 'Nevermind', 'nevermind', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (id, title, number) in [(1, "Smells Like Teen Spirit", 1), (2, "In Bloom", 2)] {
        sqlx::query(
            "INSERT INTO track
                (id, library_id, file_path, file_hash, file_size, file_modified,
                 title, album_id, primary_artist, track_number, duration_ms, added_at)
             VALUES (?, 1, ?, 'h', 0, 0, ?, 1, 1, ?, 200000, 0)",
        )
        .bind(id)
        .bind(format!("/m/{id}.flac"))
        .bind(title)
        .bind(number)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool
}

fn facts() -> AlbumFacts {
    AlbumFacts {
        release_mbid: "9dce399d-3ec7-34a4-97d0-b4bac54e5f08".into(),
        release_group_mbid: Some("1b022e01-4da6-387b-8658-8678046e4cef".into()),
        release_title: "Nevermind".into(),
        release_artist: "Nirvana".into(),
        release_country: Some("US".into()),
        release_track_count: 2,
        original_date: Some("1991-09-24".into()),
        original_year: Some(1991),
        genres: vec!["Grunge".into(), "Alternative Rock".into()],
        has_front_cover: true,
    }
}

fn matched() -> Vec<MatchedTrack> {
    (1..=2)
        .map(|id| MatchedTrack {
            track_id: id,
            recording_mbid: format!("00000000-0000-0000-0000-00000000000{id}"),
            score: 0.97,
            credits: vec![Credit {
                role: "composer".into(),
                name: "Kurt Cobain".into(),
                detail: String::new(),
                artist_mbid: None,
            }],
        })
        .collect()
}

async fn genres_of(pool: &SqlitePool, track: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT g.name FROM track_genre tg JOIN genre g ON g.id = tg.genre_id
          WHERE tg.track_id = ? ORDER BY g.name",
    )
    .bind(track)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn an_applied_match_fills_only_the_gaps() {
    let pool = pool().await;
    // Track 2 already has a genre of its own: it must keep it alone.
    sqlx::query("INSERT INTO genre (id, name, canonical_name) VALUES (9, 'Punk', 'punk')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO track_genre (track_id, genre_id) VALUES (2, 9)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE track SET year = 2011 WHERE id = 2")
        .execute(&pool)
        .await
        .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let report = record_match(
        &mut conn,
        1,
        Status::Applied,
        0.95,
        &facts(),
        &matched(),
        Fields::default(),
        1,
    )
    .await
    .unwrap();
    drop(conn);
    assert!(report.year && report.release_date && report.wants_cover);
    assert_eq!(report.genre_tracks, 1);
    assert_eq!(report.track_years, 1);

    assert_eq!(
        genres_of(&pool, 1).await,
        vec!["Alternative Rock", "Grunge"]
    );
    assert_eq!(genres_of(&pool, 2).await, vec!["Punk"]);
    let years: Vec<Option<i64>> = sqlx::query_scalar("SELECT year FROM track ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(years, vec![Some(1991), Some(2011)]);
    let (year, date): (Option<i64>, Option<String>) =
        sqlx::query_as("SELECT year, release_date FROM album WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((year, date.as_deref()), (Some(1991), Some("1991-09-24")));
    let credits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_credit")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(credits, 2);
}

#[tokio::test]
async fn a_review_writes_nothing_into_the_library() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    record_match(
        &mut conn,
        1,
        Status::Review,
        0.7,
        &facts(),
        &matched(),
        Fields::default(),
        1,
    )
    .await
    .unwrap();
    let decisions = album_decisions(&mut conn, Status::Review, 10)
        .await
        .unwrap();
    drop(conn);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].local_tracks, 2);
    assert!(genres_of(&pool, 1).await.is_empty());
    let year: Option<i64> = sqlx::query_scalar("SELECT year FROM album WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(year, None);
}

#[tokio::test]
async fn a_scan_that_empties_a_genre_is_filled_again() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    record_match(
        &mut conn,
        1,
        Status::Applied,
        0.95,
        &facts(),
        &matched(),
        Fields::default(),
        1,
    )
    .await
    .unwrap();
    // What the scanner does to a rewritten file with no genre tag.
    sqlx::query("DELETE FROM track_genre WHERE track_id = 1")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE track SET year = NULL WHERE id = 1")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(reapply_all(&mut conn, Fields::default()).await.unwrap(), 1);
    // And a second pass has nothing left to do, and writes nothing.
    assert!(!has_gap(&mut conn, 1, Fields::default()).await.unwrap());
    assert_eq!(reapply_all(&mut conn, Fields::default()).await.unwrap(), 0);
    drop(conn);
    assert_eq!(
        genres_of(&pool, 1).await,
        vec!["Alternative Rock", "Grunge"]
    );
}

#[tokio::test]
async fn undo_takes_back_what_was_filled_and_keeps_what_the_user_changed() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    record_match(
        &mut conn,
        1,
        Status::Applied,
        0.95,
        &facts(),
        &matched(),
        Fields::default(),
        1,
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO artwork (id, hash, format, source, created_at) VALUES (5, 'x', 'jpg', 'manual', 0)")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(link_cover(&mut conn, 1, 5).await.unwrap());
    // The user retypes the album's release date after the fill.
    sqlx::query("UPDATE album SET release_date = '1991-09-13' WHERE id = 1")
        .execute(&mut *conn)
        .await
        .unwrap();

    undo_album(&mut conn, 1, 2).await.unwrap();
    let (year, date, artwork): (Option<i64>, Option<String>, Option<i64>) =
        sqlx::query_as("SELECT year, release_date, artwork_id FROM album WHERE id = 1")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(year, None);
    assert_eq!(date.as_deref(), Some("1991-09-13"));
    assert_eq!(artwork, None);
    let track_year: Option<i64> = sqlx::query_scalar("SELECT year FROM track WHERE id = 1")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(track_year, None);
    let status: String =
        sqlx::query_scalar("SELECT status FROM musicbrainz_album WHERE album_id = 1")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(status, "rejected");
    // A rejected album is not pending any more.
    assert!(pending_albums(&mut conn, i64::MAX, 0, 10)
        .await
        .unwrap()
        .is_empty());
    let credits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_credit")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    assert_eq!(credits, 0);
    assert!(genres_of(&pool, 1).await.is_empty());
}

#[tokio::test]
async fn a_field_turned_off_is_never_written() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let fields = Fields {
        genres: false,
        dates: false,
        covers: false,
        credits: false,
    };
    let report = record_match(
        &mut conn,
        1,
        Status::Applied,
        0.95,
        &facts(),
        &matched(),
        fields,
        1,
    )
    .await
    .unwrap();
    drop(conn);
    assert_eq!(report, ApplyReport::default());
    assert!(genres_of(&pool, 1).await.is_empty());
    let credits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_credit")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(credits, 0);
}

#[tokio::test]
async fn no_match_is_retried_after_its_cutoff_and_never_replaces_a_decision() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    record_no_match(&mut conn, 1, 100).await.unwrap();
    assert!(pending_albums(&mut conn, 50, 0, 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        pending_albums(&mut conn, 200, 0, 10).await.unwrap().len(),
        1
    );
    assert_eq!(count_pending_albums(&mut conn, 200).await.unwrap(), 1);

    record_match(
        &mut conn,
        1,
        Status::Review,
        0.7,
        &facts(),
        &[],
        Fields::default(),
        300,
    )
    .await
    .unwrap();
    record_no_match(&mut conn, 1, 400).await.unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM musicbrainz_album WHERE album_id = 1")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(status, "review");
}

#[tokio::test]
async fn deleting_the_album_takes_its_decision_along() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    record_match(
        &mut conn,
        1,
        Status::Applied,
        0.95,
        &facts(),
        &matched(),
        Fields::default(),
        1,
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM track")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("DELETE FROM album")
        .execute(&mut *conn)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM musicbrainz_album) + (SELECT COUNT(*) FROM musicbrainz_track)
              + (SELECT COUNT(*) FROM track_credit) + (SELECT COUNT(*) FROM musicbrainz_filled_genre)",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn an_artist_proposal_waits_and_a_user_decision_is_never_replaced() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let proposal = ArtistProposal {
        mbid: "5b11f4ce-a62d-471e-81fc-a69a8278c7da".into(),
        name: "Nirvana".into(),
        disambiguation: Some("1980s–1990s US grunge band".into()),
        kind: Some("Group".into()),
        country: Some("US".into()),
        score: 0.9,
        album_evidence: 1,
    };
    propose_artist(&mut conn, 1, &proposal, 1).await.unwrap();
    propose_artist(&mut conn, 1, &proposal, 2).await.unwrap();
    let pending = artist_decisions(&mut conn, Status::Review, 10)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].album_evidence, 2,
        "the same artist gains evidence"
    );
    // Nothing shows on the artist page until the user accepts.
    assert!(artist_facts(&mut conn, 1).await.unwrap().is_none());

    let facts = ArtistFacts {
        artist_mbid: Some(proposal.mbid.clone()),
        area: Some("Aberdeen".into()),
        begin_date: Some("1987".into()),
        end_date: Some("1994-04-05".into()),
        ..Default::default()
    };
    assert!(accept_artist(&mut conn, 1, &facts, 3).await.unwrap());
    let shown = artist_facts(&mut conn, 1).await.unwrap().unwrap();
    assert_eq!(shown.area.as_deref(), Some("Aberdeen"));
    assert_eq!(shown.kind.as_deref(), Some("Group"));

    // A later pass proposing someone else changes nothing.
    let other = ArtistProposal {
        mbid: "00000000-0000-0000-0000-000000000009".into(),
        album_evidence: 5,
        ..proposal.clone()
    };
    propose_artist(&mut conn, 1, &other, 4).await.unwrap();
    let shown = artist_facts(&mut conn, 1).await.unwrap().unwrap();
    assert_eq!(shown.artist_mbid.as_deref(), Some(proposal.mbid.as_str()));
    // And it is not pending any more.
    assert!(
        pending_artists(&mut conn, i64::MAX, "Various Artists", 0, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn pending_artists_are_the_credited_ones_without_a_decision() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("INSERT INTO artist (id, name, canonical_name) VALUES (2, 'Unused', 'unused')")
        .execute(&mut *conn)
        .await
        .unwrap();
    let pending = pending_artists(&mut conn, 0, "Various Artists", 0, 10)
        .await
        .unwrap();
    assert_eq!(pending, vec![(1, "Nirvana".to_string())]);
    assert_eq!(
        count_pending_artists(&mut conn, 0, "Various Artists")
            .await
            .unwrap(),
        1
    );
    // The cursor walks past it.
    assert!(pending_artists(&mut conn, 0, "Various Artists", 1, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_cover_backlog_is_the_applied_albums_whose_release_has_a_cover() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    record_match(
        &mut conn,
        1,
        Status::Applied,
        0.95,
        &facts(),
        &matched(),
        Fields::default(),
        1,
    )
    .await
    .unwrap();
    assert_eq!(albums_wanting_cover(&mut conn).await.unwrap().len(), 1);
    // A release the archive turned out to hold no front cover for is
    // never retried.
    forget_front_cover(&mut conn, 1).await.unwrap();
    assert!(albums_wanting_cover(&mut conn).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_value_the_user_wrote_since_survives_the_undo() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    record_match(
        &mut conn,
        1,
        Status::Applied,
        0.95,
        &facts(),
        &matched(),
        Fields::default(),
        1,
    )
    .await
    .unwrap();
    // The tag editor saves track 1, keeping the filled genres and year.
    release_track_fields(&mut conn, 1, true, true)
        .await
        .unwrap();
    undo_album(&mut conn, 1, 2).await.unwrap();
    let year: Option<i64> = sqlx::query_scalar("SELECT year FROM track WHERE id = 1")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    assert_eq!(year, Some(1991));
    assert_eq!(
        genres_of(&pool, 1).await,
        vec!["Alternative Rock", "Grunge"]
    );
    // Track 2 was not edited: its fill is taken back.
    assert!(genres_of(&pool, 2).await.is_empty());
}

async fn album_year_filled(conn: &mut sqlx::SqliteConnection) -> bool {
    sqlx::query_scalar("SELECT filled_year FROM musicbrainz_album WHERE album_id = 1")
        .fetch_one(conn)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_album_year_is_released_only_when_it_is_the_typed_one() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    record_match(
        &mut conn,
        1,
        Status::Applied,
        0.95,
        &facts(),
        &matched(),
        Fields::default(),
        1,
    )
    .await
    .unwrap();
    // Another track typed 2011; the album keeps the filled 1991.
    release_album_years(&mut conn, &[1], 2011).await.unwrap();
    assert!(album_year_filled(&mut conn).await);
    // The typed year is the album's: it is the user's now.
    release_album_years(&mut conn, &[1], 1991).await.unwrap();
    assert!(!album_year_filled(&mut conn).await);
}
