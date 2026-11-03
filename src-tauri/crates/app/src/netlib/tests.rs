//! The scanner, the queue and the cache over a share that answers what the
//! test tells it to. The real migrations and the real `scan_folder_inner`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use sqlx::SqlitePool;
use tokio::sync::Mutex as AsyncMutex;

use super::fs::{Entry, NetError, NetworkFs};
use super::*;
use crate::commands::scan::scan_folder_inner;

/// The registry is process-wide, as in production, so these tests take turns.
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

const UUID: &str = "11111111-aaaa";
const LIBRARY_ROOT: &str = "wfnet://11111111-aaaa/Music";

#[derive(Default)]
struct FakeState {
    files: Mutex<HashMap<String, (Vec<u8>, i64)>>,
    down: AtomicBool,
    /// Every directory lists as empty, a volume the server has not mounted.
    empty: AtomicBool,
    /// A directory this account may not read.
    refused_dir: Mutex<Option<String>>,
    /// A directory its parent names and that then answers not found.
    gone_dir: Mutex<Option<String>>,
    /// The account as a whole is refused.
    bad_password: AtomicBool,
    audio_bytes: AtomicU64,
}

struct FakeFs(Arc<FakeState>);

impl FakeFs {
    fn check(&self) -> Result<(), NetError> {
        if self.0.down.load(Ordering::Acquire) {
            return Err(NetError::Unreachable("refused".into()));
        }
        Ok(())
    }
}

impl NetworkFs for FakeFs {
    fn probe(&self) -> BoxFuture<'_, Result<(), NetError>> {
        Box::pin(async move {
            self.check()?;
            if self.0.bad_password.load(Ordering::Acquire) {
                return Err(NetError::Auth("wrong password".into()));
            }
            Ok(())
        })
    }

    fn list<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, Result<Vec<Entry>, NetError>> {
        Box::pin(async move {
            self.check()?;
            if self.0.refused_dir.lock().unwrap().as_deref() == Some(dir) {
                return Err(NetError::Auth("no access here".into()));
            }
            if self.0.gone_dir.lock().unwrap().as_deref() == Some(dir) {
                return Err(NetError::NotFound);
            }
            if self.0.empty.load(Ordering::Acquire) {
                return Ok(Vec::new());
            }
            let prefix = format!("{dir}/");
            let files = self.0.files.lock().unwrap();
            let mut dirs = std::collections::BTreeSet::new();
            let mut out = Vec::new();
            for (path, (bytes, modified_ms)) in files.iter() {
                let Some(rest) = path.strip_prefix(&prefix) else {
                    continue;
                };
                match rest.split_once('/') {
                    Some((sub, _)) => {
                        dirs.insert(sub.to_string());
                    }
                    None => out.push(Entry {
                        name: rest.to_string(),
                        is_dir: false,
                        size: bytes.len() as u64,
                        modified_ms: *modified_ms,
                    }),
                }
            }
            out.extend(dirs.into_iter().map(|name| Entry {
                name,
                is_dir: true,
                size: 0,
                modified_ms: 0,
            }));
            Ok(out)
        })
    }

    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Entry, NetError>> {
        Box::pin(async move {
            self.check()?;
            let files = self.0.files.lock().unwrap();
            let (bytes, modified_ms) = files.get(path).ok_or(NetError::NotFound)?;
            Ok(Entry {
                name: path.rsplit('/').next().unwrap().to_string(),
                is_dir: false,
                size: bytes.len() as u64,
                modified_ms: *modified_ms,
            })
        })
    }

    fn read_at<'a>(
        &'a self,
        path: &'a str,
        offset: u64,
        len: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, NetError>> {
        Box::pin(async move {
            self.check()?;
            let files = self.0.files.lock().unwrap();
            let (bytes, _) = files.get(path).ok_or(NetError::NotFound)?;
            let start = (offset as usize).min(bytes.len());
            let end = (start + len as usize).min(bytes.len());
            self.0
                .audio_bytes
                .fetch_add((end - start) as u64, Ordering::Relaxed);
            Ok(bytes[start..end].to_vec())
        })
    }
}

/// A short valid WAV, big enough for the partial hash to apply.
fn wav(fill: u8) -> Vec<u8> {
    let data_len: u32 = 3 * 1024 * 1024;
    let mut out = Vec::with_capacity(data_len as usize + 44);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&44_100_u32.to_le_bytes());
    out.extend_from_slice(&176_400_u32.to_le_bytes());
    out.extend_from_slice(&4_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.resize(44 + data_len as usize, fill);
    out
}

struct Bench {
    pool: SqlitePool,
    state: Arc<FakeState>,
    dir: tempfile::TempDir,
    folder_id: i64,
}

impl Bench {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let options = waveflow_core::repository::sqlite::collation::register(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(dir.path().join("profile.db"))
                .create_if_missing(true)
                .foreign_keys(true),
        );
        let pool = SqlitePool::connect_with(options).await.unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0)")
            .execute(&pool)
            .await
            .unwrap();
        let folder_id = sqlx::query("INSERT INTO library_folder (library_id, path) VALUES (1, ?)")
            .bind(LIBRARY_ROOT)
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();

        let state = Arc::new(FakeState::default());
        let source = Arc::new(Source {
            uuid: UUID.to_string(),
            name: "NAS".to_string(),
            fs: Box::new(FakeFs(Arc::clone(&state))),
            state: Mutex::new((SourceState::Unknown, None)),
            checking_access: AtomicBool::new(false),
        });
        {
            let mut registry = registry().write().unwrap();
            registry.sources = HashMap::from([(UUID.to_string(), source)]);
            registry.cache_dir = Some(dir.path().join("cache"));
            registry.cache_max_bytes = 1024 * 1024 * 1024;
        }
        Self {
            pool,
            state,
            dir,
            folder_id,
        }
    }

    fn put(&self, path: &str, bytes: Vec<u8>, modified_ms: i64) {
        self.state
            .files
            .lock()
            .unwrap()
            .insert(path.to_string(), (bytes, modified_ms));
    }

    async fn scan(&self) -> crate::error::AppResult<crate::commands::scan::ScanSummary> {
        let artwork = self.dir.path().join("artwork");
        scan_folder_inner(&self.pool, &artwork, self.folder_id, None, false).await
    }

    async fn available(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT file_path FROM track WHERE is_available = 1 ORDER BY file_path")
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_first_scan_makes_tracks_and_a_digest_equal_to_a_local_copy() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    let bytes = wav(7);
    bench.put(
        "Music/Rock/AC#DC ? 100%.wav",
        bytes.clone(),
        1_700_000_000_000,
    );
    bench.put("Music/notes.txt", b"not music".to_vec(), 1);
    bench.put("Music/.waveflow/canvas/clip.mp4", wav(9), 1);
    bench.put("Music/Rock/._AC#DC.wav", wav(9), 1);

    let summary = bench.scan().await.unwrap();
    assert_eq!(summary.added, 1, "errors: {}", summary.errors);
    assert_eq!(
        bench.available().await,
        vec!["wfnet://11111111-aaaa/Music/Rock/AC#DC ? 100%.wav"]
    );

    let (hash, size, modified, title): (String, i64, i64, String) =
        sqlx::query_as("SELECT file_hash, file_size, file_modified, title FROM track")
            .fetch_one(&bench.pool)
            .await
            .unwrap();
    let local = bench.dir.path().join("copy.wav");
    std::fs::write(&local, &bytes).unwrap();
    assert_eq!(hash, waveflow_core::scanner::hash_file(&local).unwrap());
    assert_eq!(size, bytes.len() as i64);
    assert_eq!(modified, 1_700_000_000_000);
    assert_eq!(title, "AC#DC ? 100%", "the name, not an encoding of it");

    // The two hash windows, and the tags served from them.
    let read = bench.state.audio_bytes.load(Ordering::Relaxed);
    assert_eq!(read, 2 * 1024 * 1024, "read {read} bytes for a 3 MiB file");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rescan_that_finds_nothing_changed_reads_no_audio() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1_700_000_000_000);
    bench.scan().await.unwrap();

    let before = bench.state.audio_bytes.load(Ordering::Relaxed);
    let summary = bench.scan().await.unwrap();
    assert_eq!((summary.added, summary.updated, summary.skipped), (0, 0, 1));
    assert_eq!(bench.state.audio_bytes.load(Ordering::Relaxed), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_share_that_does_not_answer_is_not_an_empty_folder() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1);
    bench.put("Music/b.wav", wav(2), 1);
    bench.scan().await.unwrap();
    assert_eq!(bench.available().await.len(), 2);

    bench.state.down.store(true, Ordering::Release);
    assert!(bench.scan().await.is_err());
    assert_eq!(bench.available().await.len(), 2, "nothing swept");
    assert_eq!(
        source(UUID).unwrap().state(),
        SourceState::Unreachable,
        "and the rows can turn grey"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_listing_does_not_hide_a_library() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1);
    bench.scan().await.unwrap();

    bench.state.empty.store(true, Ordering::Release);
    assert!(bench.scan().await.is_err());
    assert_eq!(bench.available().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_gone_from_a_whole_listing_is_gone_and_keeps_its_row() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1);
    bench.put("Music/b.wav", wav(2), 1);
    bench.scan().await.unwrap();

    bench.state.files.lock().unwrap().remove("Music/b.wav");
    let summary = bench.scan().await.unwrap();
    assert_eq!(summary.removed, 1);
    assert_eq!(bench.available().await.len(), 1);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track")
        .fetch_one(&bench.pool)
        .await
        .unwrap();
    assert_eq!(rows, 2);
}

/// A system folder on a Windows share: the rest is listed, nothing under the
/// refused folder is swept, and the share is not marked as refusing us.
#[tokio::test(flavor = "multi_thread")]
async fn a_folder_the_account_cannot_read_stops_nothing_and_loses_nothing() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1);
    bench.put("Music/private/b.wav", wav(2), 1);
    bench.scan().await.unwrap();
    assert_eq!(bench.available().await.len(), 2);

    *bench.state.refused_dir.lock().unwrap() = Some("Music/private".to_string());
    bench.put("Music/c.wav", wav(3), 1);
    let summary = bench.scan().await.unwrap();
    assert_eq!((summary.added, summary.removed), (1, 0));
    assert_eq!(bench.available().await.len(), 3, "b.wav was not swept");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_ne!(source(UUID).unwrap().state(), SourceState::AuthFailed);
}

/// A folder its parent still names and that answers not found says nothing of
/// its tracks. They go on the scan where the parent stops naming it.
#[tokio::test(flavor = "multi_thread")]
async fn a_folder_that_vanishes_between_two_listings_loses_nothing_yet() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1);
    bench.put("Music/album/b.wav", wav(2), 1);
    bench.scan().await.unwrap();
    assert_eq!(bench.available().await.len(), 2);

    *bench.state.gone_dir.lock().unwrap() = Some("Music/album".to_string());
    let summary = bench.scan().await.unwrap();
    assert_eq!(summary.removed, 0);
    assert_eq!(bench.available().await.len(), 2, "b.wav was not swept");

    *bench.state.gone_dir.lock().unwrap() = None;
    bench
        .state
        .files
        .lock()
        .unwrap()
        .remove("Music/album/b.wav");
    let summary = bench.scan().await.unwrap();
    assert_eq!(summary.removed, 1);
    assert_eq!(bench.available().await.len(), 1);
}

/// Refused at the root is another matter, and only signing in decides.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_sign_in_marks_the_source() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1);
    bench.scan().await.unwrap();

    bench.state.bad_password.store(true, Ordering::Release);
    *bench.state.refused_dir.lock().unwrap() = Some("Music".to_string());
    assert!(bench.scan().await.is_err());
    assert_eq!(bench.available().await.len(), 1, "nothing swept");
    for _ in 0..100 {
        if source(UUID).unwrap().state() == SourceState::AuthFailed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(source(UUID).unwrap().state(), SourceState::AuthFailed);
    assert!(source(UUID).unwrap().is_down());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rating_set_here_survives_the_file_changing_on_the_share() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1_000);
    bench.scan().await.unwrap();
    sqlx::query("UPDATE track SET rating = 204")
        .execute(&bench.pool)
        .await
        .unwrap();

    bench.put("Music/a.wav", wav(2), 90_000);
    let summary = bench.scan().await.unwrap();
    assert_eq!(summary.updated, 1);
    let rating: Option<i64> = sqlx::query_scalar("SELECT rating FROM track")
        .fetch_one(&bench.pool)
        .await
        .unwrap();
    assert_eq!(rating, Some(204));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_queue_steps_over_what_cannot_play_and_ends_when_nothing_can() {
    use crate::queue::{self, RepeatMode};

    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/1.wav", wav(1), 1);
    bench.put("Music/2.wav", wav(2), 1);
    bench.put("Music/3.wav", wav(3), 1);
    bench.scan().await.unwrap();
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM track ORDER BY file_path")
        .fetch_all(&bench.pool)
        .await
        .unwrap();
    queue::fill_queue(&bench.pool, "album", None, &ids, 0, false)
        .await
        .unwrap();

    let next = |pool: SqlitePool| async move {
        queue::peek_next(&pool, RepeatMode::Off)
            .await
            .unwrap()
            .map(|t| t.file_path)
    };
    assert_eq!(
        next(bench.pool.clone()).await.as_deref(),
        Some("wfnet://11111111-aaaa/Music/2.wav")
    );

    // Down, with a copy of the third alone: the second is stepped over.
    source(UUID)
        .unwrap()
        .set_state(SourceState::Unreachable, None);
    let third = "wfnet://11111111-aaaa/Music/3.wav";
    let cache = bench.dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join(cache::entry_name(third).unwrap()), b"copy").unwrap();
    assert!(is_blocked("wfnet://11111111-aaaa/Music/2.wav"));
    assert!(!is_blocked(third));
    assert_eq!(next(bench.pool.clone()).await.as_deref(), Some(third));

    // No copy left: the queue ends, it does not spin.
    std::fs::remove_file(cache.join(cache::entry_name(third).unwrap())).unwrap();
    assert_eq!(next(bench.pool.clone()).await, None);

    // And what is grey comes from the same answer.
    assert_eq!(blocked_track_ids(&bench.pool).await.unwrap().len(), 3);
    source(UUID).unwrap().set_state(SourceState::Online, None);
    assert!(blocked_track_ids(&bench.pool).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn playback_keeps_what_it_reads_and_plays_it_without_the_share() {
    use std::io::Read;

    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    let bytes = wav(5);
    bench.put("Music/a.wav", bytes.clone(), 1_700_000_000_000);
    let locator = "wfnet://11111111-aaaa/Music/a.wav";

    let locator_owned = locator.to_string();
    let read_all = move || -> Vec<u8> {
        let mut source = media::open(&locator_owned).unwrap();
        let mut out = Vec::new();
        source.read_to_end(&mut out).unwrap();
        out
    };
    let first = {
        let read_all = read_all.clone();
        tokio::task::spawn_blocking(read_all).await.unwrap()
    };
    assert_eq!(first, bytes);

    // Published once the whole file went through; the transfer ends just
    // after the last read, on its own thread.
    let mut slot = None;
    for _ in 0..100 {
        slot = cache::slot(locator);
        if slot.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let slot = slot.expect("the copy was published");
    assert!(slot.matches(bytes.len() as u64, 1_700_000_000_000));

    bench.state.down.store(true, Ordering::Release);
    source(UUID)
        .unwrap()
        .set_state(SourceState::Unreachable, None);
    let again = tokio::task::spawn_blocking(read_all).await.unwrap();
    assert_eq!(again, bytes);
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_goes_out_in_offline_mode() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1);
    let result = {
        // Reset by drop: a panic here must not leave the next test offline.
        struct Offline;
        impl Drop for Offline {
            fn drop(&mut self) {
                crate::offline::set(false);
            }
        }
        crate::offline::set(true);
        let _offline = Offline;
        bench.scan().await
    };
    assert!(result.is_err());
    assert_eq!(bench.state.audio_bytes.load(Ordering::Relaxed), 0);
}

/// A first scan stopped half way does not read everything again: the one-off
/// passes for rows older than a feature do not run on a share.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_scan_after_a_failed_file_reads_only_what_is_new() {
    let _turn = SERIAL.lock().await;
    let bench = Bench::new().await;
    bench.put("Music/a.wav", wav(1), 1);
    bench.put("Music/broken.wav", vec![0; 16], 1);
    let first = bench.scan().await.unwrap();
    assert_eq!(first.errors, 1);

    let before = bench.state.audio_bytes.load(Ordering::Relaxed);
    bench.scan().await.unwrap();
    let read = bench.state.audio_bytes.load(Ordering::Relaxed) - before;
    assert!(read <= 16, "read {read} bytes again");
}

#[test]
fn a_track_on_a_share_is_never_written() {
    assert!(refuse_if_network("wfnet://abc/a.flac").is_err());
    assert!(refuse_if_network("/music/a.flac").is_ok());
}
