use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

/// The real profile schema with the app's collations, foreign keys on.
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
    sqlx::query("INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0)")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

/// A track, and its analysis when `analysis` is `Some((cutoff, version, at))`.
async fn track(
    pool: &SqlitePool,
    id: i64,
    title: &str,
    codec: &str,
    depth: Option<i64>,
    available: bool,
    analysis: Option<(Option<f64>, Option<i64>, i64)>,
) {
    let ext = codec.to_lowercase();
    sqlx::query(
        "INSERT INTO track
            (id, library_id, file_path, file_hash, file_size, file_modified,
             title, duration_ms, added_at, codec, bit_depth, sample_rate, is_available)
         VALUES (?, 1, ?, 'h', 0, 0, ?, 1000, 0, ?, ?, 44100, ?)",
    )
    .bind(id)
    .bind(format!("/music/{id}.{ext}"))
    .bind(title)
    .bind(codec)
    .bind(depth)
    .bind(available)
    .execute(pool)
    .await
    .unwrap();
    if let Some((cutoff, version, at)) = analysis {
        sqlx::query(
            "INSERT INTO track_analysis (track_id, cutoff_hz, bandwidth_version, analyzed_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(cutoff)
        .bind(version)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// Two fakes at different walls, a genuine FLAC, one waiting, an MP3 and
/// a DSD file — and a fake whose file has gone, which nothing counts.
async fn seed(pool: &SqlitePool) {
    track(
        pool,
        1,
        "Clean",
        "FLAC",
        Some(16),
        true,
        Some((None, Some(1), 30)),
    )
    .await;
    track(
        pool,
        2,
        "Fake high",
        "FLAC",
        Some(24),
        true,
        Some((Some(19_800.0), Some(1), 40)),
    )
    .await;
    track(
        pool,
        3,
        "Fake low",
        "WAV",
        Some(16),
        true,
        Some((Some(16_000.0), Some(1), 20)),
    )
    .await;
    track(pool, 4, "Waiting", "FLAC", Some(16), true, None).await;
    track(
        pool,
        5,
        "Song",
        "MP3",
        None,
        true,
        Some((Some(16_000.0), Some(1), 50)),
    )
    .await;
    track(pool, 6, "Dsd", "DSD64", Some(1), true, None).await;
    track(
        pool,
        7,
        "Gone",
        "FLAC",
        Some(16),
        false,
        Some((Some(16_000.0), Some(1), 10)),
    )
    .await;
}

fn count(summary: &QualitySummary, verdict: &str) -> i64 {
    summary
        .verdicts
        .iter()
        .find(|c| c.verdict == verdict)
        .map(|c| c.count)
        .unwrap()
}

#[tokio::test]
async fn the_summary_counts_every_verdict_over_available_files() {
    let pool = pool().await;
    seed(&pool).await;
    let summary = read_summary(&pool).await.unwrap();
    assert_eq!(summary.verdicts.len(), QUALITY_VERDICTS.len());
    assert_eq!(count(&summary, "lossy_source"), 2);
    assert_eq!(count(&summary, "pending"), 1);
    assert_eq!(count(&summary, "consistent"), 1);
    assert_eq!(count(&summary, "lossy"), 1);
    assert_eq!(count(&summary, "not_measured"), 1);
    assert_eq!(summary.lossless, 4);
    assert_eq!(summary.measured, 3);
    // The MP3's analysis is not a lossless file's, and the gone file is
    // not counted at all.
    assert_eq!(summary.last_analyzed_at, Some(40));
}

#[tokio::test]
async fn the_list_puts_the_likeliest_fakes_first() {
    let pool = pool().await;
    seed(&pool).await;
    let page = read_page(&pool, PathBuf::from("/art"), None, None, 0, 100)
        .await
        .unwrap();
    assert_eq!(page.total, 6);
    let ids: Vec<i64> = page.items.iter().map(|r| r.id).collect();
    // The lowest wall first, then by verdict in `QUALITY_VERDICTS` order.
    assert_eq!(ids, vec![3, 2, 4, 1, 5, 6]);
}

#[tokio::test]
async fn a_page_is_a_window_on_the_filter_and_the_total_is_the_filters() {
    let pool = pool().await;
    seed(&pool).await;
    let page = read_page(
        &pool,
        PathBuf::from("/art"),
        Some("lossy_source"),
        None,
        1,
        1,
    )
    .await
    .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, 2);
    assert_eq!(page.items[0].cutoff_hz, Some(19_800.0));

    let wav = read_page(&pool, PathBuf::from("/art"), None, Some("WAV"), 0, 100)
        .await
        .unwrap();
    assert_eq!(wav.total, 1);
    assert_eq!(wav.items[0].id, 3);

    let none = read_page(
        &pool,
        PathBuf::from("/art"),
        Some("pending"),
        Some("WAV"),
        0,
        100,
    )
    .await
    .unwrap();
    assert_eq!(none.total, 0);
    assert!(none.items.is_empty());
}

#[tokio::test]
async fn an_unknown_verdict_is_refused() {
    let pool = pool().await;
    let err = read_page(&pool, PathBuf::from("/art"), Some("genuine"), None, 0, 10).await;
    assert!(err.is_err());
}

#[tokio::test]
async fn analysing_again_marks_the_lossless_files_and_nothing_else() {
    let pool = pool().await;
    seed(&pool).await;
    let marked = super::super::analysis::mark_lossless_unmeasured(&pool)
        .await
        .unwrap();
    // The three measured lossless files that are still there.
    assert_eq!(marked, 3);
    let summary = read_summary(&pool).await.unwrap();
    assert_eq!(count(&summary, "pending"), 4);
    assert_eq!(summary.measured, 0);
    // The MP3 keeps its measure, and every cutoff stays stored.
    let kept: Vec<(i64, Option<i64>, Option<f64>)> = sqlx::query_as(
        "SELECT track_id, bandwidth_version, cutoff_hz FROM track_analysis ORDER BY track_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        kept,
        vec![
            (1, None, None),
            (2, None, Some(19_800.0)),
            (3, None, Some(16_000.0)),
            (5, Some(1), Some(16_000.0)),
            (7, Some(1), Some(16_000.0)),
        ]
    );
}

#[test]
fn the_summary_arithmetic() {
    let counts: HashMap<String, i64> = [("lossy_source", 2), ("pending", 5), ("lossy", 9)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let summary = summarize(&counts, None);
    assert_eq!(summary.lossless, 7);
    assert_eq!(summary.measured, 2);
    assert_eq!(count(&summary, "consistent"), 0);
}
