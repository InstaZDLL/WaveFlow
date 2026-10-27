use super::*;

#[test]
fn an_extension_is_matched_the_way_the_scanner_matches_it() {
    assert_eq!(extension_of("/m/a.FLAC"), "flac");
    assert_eq!(extension_of("/m/a"), "");
    assert!(waveflow_core::scanner::AUDIO_EXTENSIONS.contains(&extension_of("/m/a.Mp3").as_str()));
}

#[test]
fn a_cancel_does_not_survive_the_run_that_saw_it() {
    let guard = RunGuard::claim().expect("the slot is free");
    request_cancel();
    assert!(cancelled());
    // A second sweep cannot start while this one holds the slot, which is
    // what keeps one run from clearing the other's stop request.
    assert!(RunGuard::claim().is_none());
    drop(guard);
    assert!(!cancelled(), "a cancel must not poison the next sweep");
    assert!(RunGuard::claim().is_some(), "the slot must be free again");
}

/// The real migrator against a real database, so the columns are the ones
/// that ship rather than a fixture's idea of them.
async fn pool() -> sqlx::SqlitePool {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let options = SqliteConnectOptions::from_str(":memory:")
        .unwrap()
        .foreign_keys(true);
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

/// The survey's per-candidate read, run against the shipped schema.
///
/// It asked `track` for `artist_display`, a column no migration has ever
/// created — the name is `primary_artist` — so the sweep died on its first
/// candidate and the whole upload feature was unreachable. Nothing could
/// catch it before it ran: `sqlx::query` is the unchecked form, so neither
/// `cargo check` nor clippy reads the string, and no test in this module
/// touched a database.
///
/// And `primary_artist` is a rowid into `artist`, not a name: reading it
/// as text would have labelled every candidate with a number. The join is
/// what this asserts, against the shipped schema rather than a fixture's
/// idea of it.
#[tokio::test]
async fn the_survey_reads_a_name_not_an_artist_rowid() {
    let pool = pool().await;
    // One statement per call: batched through `raw_sql`, a row can land
    // before the row it references exists and the foreign key refuses it.
    for statement in [
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        "INSERT INTO artist (id, name, canonical_name) VALUES (7, 'NAYEON', 'nayeon')",
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, primary_artist, duration_ms, added_at, is_available,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/m/a.flac', 'h', 1, 0, 'T', 7, 1000, 0, 1, 0, 0, 0, 0)",
    ] {
        sqlx::raw_sql(statement).execute(&pool).await.unwrap();
    }

    let row = sqlx::query(
        "SELECT t.title AS title, ar.name AS artist_name
               FROM track t
               LEFT JOIN artist ar ON ar.id = t.primary_artist
              WHERE t.id = ?",
    )
    .bind(1_i64)
    .fetch_optional(&pool)
    .await
    .expect("the survey's read must run against the shipped schema")
    .expect("the seeded track");
    assert_eq!(row.try_get::<String, _>("title").unwrap(), "T");
    assert_eq!(
        row.try_get::<Option<String>, _>("artist_name").unwrap(),
        Some("NAYEON".into()),
        "the candidate is labelled with a name, not a rowid",
    );
}
