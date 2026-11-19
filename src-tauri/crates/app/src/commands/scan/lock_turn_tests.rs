//! A scan must not keep the rest of the app from writing. Clicking a track
//! during a scan replaces the queue, and that write waited behind the scan's
//! batches until its busy timeout ran out; when it did get in between two
//! batches, the scan's next batch failed on its stale snapshot and the whole
//! scan with it. Through the real scan and the real queue write, on the
//! migrated schema, with real files.

use super::scan_folder_inner;
use lofty::config::WriteOptions;
use lofty::ogg::tag::VorbisComments;
use lofty::tag::TagExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::time::{Duration, Instant};

/// Enough files for the scan to run for several batches of 200, and for a
/// few seconds even on a fast machine (a small FLAC costs about a
/// millisecond there).
const FILES: usize = 4_000;

/// How long a queue write may take while a scan runs. A scan that lets a
/// waiter in takes about one file's writes plus one sleep of the waiter's
/// busy handler (at most 100 ms); a scan that does not keeps it waiting for
/// a whole batch, or for its busy timeout.
const WRITE_BUDGET: Duration = Duration::from_millis(1_500);

async fn profile_pool(dir: &Path) -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(dir.join("data.db"))
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        // As the profile pool does (`db::profile_db::open`).
        .after_connect(|conn, _meta| Box::pin(crate::db::lock_waiters::install(conn)))
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                              hlc_wall, hlc_logical)
         VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let root = dir.join("music");
    std::fs::create_dir_all(&root).unwrap();
    sqlx::query(
        "INSERT INTO library_folder (id, library_id, path, last_scanned_at, is_watched)
         VALUES (1, 1, ?, 0, 0)",
    )
    .bind(root.to_string_lossy().to_string())
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// `FILES` real FLACs, ten to an album, an artist each.
fn write_library(root: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tags/silence.flac");
    for i in 0..FILES {
        let album = i / 10;
        let dir = root.join(format!("Album {album}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{i:04}.flac"));
        std::fs::copy(&fixture, &path).unwrap();
        let mut tag = VorbisComments::default();
        for (key, value) in [
            ("TITLE", format!("Track {i}")),
            ("ARTIST", format!("Artist {i}")),
            ("ALBUM", format!("Album {album}")),
            ("ALBUMARTIST", format!("Album Artist {album}")),
            ("TRACKNUMBER", format!("{}", i % 10 + 1)),
            ("GENRE", format!("Genre {}", i % 7)),
        ] {
            tag.push(key.to_string(), value);
        }
        tag.save_to_path(&path, WriteOptions::default())
            .expect("write comments");
    }
}

async fn track_ids(pool: &SqlitePool) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM track ORDER BY id LIMIT 10")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queue_write_gets_through_a_scan_and_the_scan_finishes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = profile_pool(dir.path()).await;
    write_library(&dir.path().join("music"));

    let scan = tokio::spawn({
        let pool = pool.clone();
        let artwork = dir.path().join("artwork");
        async move { scan_folder_inner(&pool, &artwork, 1, None, false).await }
    });

    // The first tracks are visible once the scan's first batch is committed.
    let started = Instant::now();
    let ids = loop {
        let ids = track_ids(&pool).await;
        if !ids.is_empty() {
            break ids;
        }
        assert!(
            !scan.is_finished(),
            "the scan ended before writing anything"
        );
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "no track was ever written"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    // Clicks on a track, the way the player writes them, while it scans.
    let mut during_scan = 0;
    let mut slowest = Duration::ZERO;
    while !scan.is_finished() && during_scan < 20 {
        let began = Instant::now();
        crate::queue::fill_queue(&pool, "library", None, &ids, 0, false)
            .await
            .expect("a queue written during a scan");
        let took = began.elapsed();
        slowest = slowest.max(took);
        assert!(
            took < WRITE_BUDGET,
            "a queue write waited {took:?} behind the scan"
        );
        during_scan += 1;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Whether the scan lived through them first: one that failed on the
    // first write also ends the loop early.
    let summary = scan
        .await
        .unwrap()
        .expect("the scan survives the writes made during it");
    assert_eq!(summary.added as usize, FILES, "{summary:?}");
    assert_eq!(summary.errors, 0);
    assert!(
        during_scan >= 3,
        "only {during_scan} writes happened while the scan ran ({:?}); too fast to tell",
        started.elapsed()
    );
    eprintln!("{during_scan} queue writes during the scan, slowest {slowest:?}");
}
