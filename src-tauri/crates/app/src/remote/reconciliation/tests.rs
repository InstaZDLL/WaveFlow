use super::playlists::convert_playlist_with_post_validation;
use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::fs;
use std::str::FromStr;
use std::sync::{Arc, Barrier};

/// Serializes the tests that mutate the process-global `RECONCILE_PHASE`, so
/// cargo's parallel runner can't interleave them. A tokio mutex (not `std`)
/// because these tests hold it across `.await`. Tests on the non-cancellable
/// path never read the phase and so don't need it.
static PHASE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Arbitration of the cancel-vs-commit race the phase machine exists for.
#[tokio::test]
async fn cancel_and_commit_transitions_are_mutually_exclusive() {
    let _serial = PHASE_TEST_LOCK.lock().await;
    // Barrier-synchronised race: `request_cancel` (STAGING → CANCELLED) and
    // the pre-commit transition (STAGING → COMMITTING) released at the same
    // instant. Exactly one must win each round — never both (a persist while
    // `request_cancel` also reported success) and never neither.
    for _ in 0..20_000 {
        RECONCILE_PHASE.store(PHASE_STAGING, Ordering::SeqCst);
        let barrier = Arc::new(Barrier::new(2));

        let cancel_barrier = Arc::clone(&barrier);
        let cancel = std::thread::spawn(move || {
            cancel_barrier.wait();
            request_cancel()
        });
        let commit_barrier = Arc::clone(&barrier);
        let commit = std::thread::spawn(move || {
            commit_barrier.wait();
            RECONCILE_PHASE
                .compare_exchange(
                    PHASE_STAGING,
                    PHASE_COMMITTING,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
        });

        let cancelled = cancel.join().unwrap();
        let committed = commit.join().unwrap();
        assert!(
            cancelled ^ committed,
            "exactly one of cancel/commit must win (cancelled={cancelled}, committed={committed})"
        );
    }

    // Once committing, or when idle, a cancel is too late / a no-op and must
    // report the authoritative `false`.
    RECONCILE_PHASE.store(PHASE_COMMITTING, Ordering::SeqCst);
    assert!(
        !request_cancel(),
        "cancel after commit started must be rejected"
    );
    RECONCILE_PHASE.store(PHASE_IDLE, Ordering::SeqCst);
    assert!(
        !request_cancel(),
        "cancel with nothing running must be rejected"
    );
}

/// A cancel latched while the scan is still staging stops the hashing loop
/// before any file is read and persists nothing — exercised with no
/// `AppHandle`, via the explicit `cancellable` flag.
#[tokio::test]
async fn cancel_during_hashing_persists_nothing() {
    let _serial = PHASE_TEST_LOCK.lock().await;
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.flac");
    fs::write(&path, b"same bytes").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    insert_remote(&pool, "remote-1", b"same bytes", "Remote").await;

    RECONCILE_PHASE.store(PHASE_CANCELLED, Ordering::SeqCst);
    let report = discover_inner(&pool, None, true).await.unwrap();
    assert!(report.cancelled);
    assert_eq!(report.auto_linked, 0);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_track_link")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "a cancelled hash must not persist any link");
    RECONCILE_PHASE.store(PHASE_IDLE, Ordering::SeqCst);
}

/// The empty-remote early return is a terminal point too: a cancel that
/// raced it must be reported as cancelled, not as a plain empty result.
#[tokio::test]
async fn cancel_with_empty_remote_reports_cancelled() {
    let _serial = PHASE_TEST_LOCK.lock().await;
    // `pool()` leaves `remote_track` empty, so `discover_inner` takes the
    // early return without hashing or reconciling.
    let pool = pool().await;
    RECONCILE_PHASE.store(PHASE_CANCELLED, Ordering::SeqCst);
    let report = discover_inner(&pool, None, true).await.unwrap();
    assert!(
        report.cancelled,
        "a cancel racing the empty-remote path is reported"
    );
    RECONCILE_PHASE.store(PHASE_IDLE, Ordering::SeqCst);
}

/// The sweep answers from the shared digest cache instead of reading the
/// file again — which is what makes RFC-006's completeness frontier mean
/// "examined" rather than "examined by whichever pass ran last".
///
/// Proved by deleting the file after seeding the cache: a sweep that still
/// finds the digest cannot have read it.
#[tokio::test]
async fn the_sweep_reads_the_cache_rather_than_the_file() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.flac");
    fs::write(&path, b"same bytes").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    insert_remote(&pool, "remote-1", b"same bytes", "Remote").await;

    let locals = load_local_candidates(&pool).await.unwrap();
    let ids: Vec<i64> = locals.iter().map(|track| track.id).collect();
    let scan = hash_local_tracks(locals, HashMap::new(), None, None, 0, false);
    assert_eq!(scan.computed.len(), 1, "the first sweep reads the file");
    super::super::hashing::remember(&pool, &scan.computed)
        .await
        .unwrap();

    fs::remove_file(&path).unwrap();
    let known = super::super::hashing::cached_digests(&pool, &ids)
        .await
        .unwrap();
    let locals = load_local_candidates(&pool).await.unwrap();
    let second = hash_local_tracks(locals, known, None, None, 0, false);
    assert_eq!(
        second.hashed.len(),
        1,
        "the file is gone, the digest is not"
    );
    assert_eq!(second.unreadable, 0);
    assert!(
        second.computed.is_empty(),
        "nothing was read, so nothing is new to file"
    );
}

/// A cancel that lands after hashing finished, while `reconcile` is staging
/// its writes, loses the pre-commit `STAGING → COMMITTING` transition and
/// rolls the transaction back before commit, so no link row is written.
#[tokio::test]
async fn cancel_during_reconcile_rolls_back_before_commit() {
    let _serial = PHASE_TEST_LOCK.lock().await;
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.flac");
    fs::write(&path, b"same bytes").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    insert_remote(&pool, "remote-1", b"same bytes", "Remote").await;

    // Hash to completion on the non-cancellable path (never reads the phase),
    // so the cancel below can only be caught by reconcile's pre-commit CAS.
    let locals = load_local_candidates(&pool).await.unwrap();
    let scan = hash_local_tracks(locals, HashMap::new(), None, None, 0, false);
    assert_eq!(scan.hashed.len(), 1);
    let remotes = load_remote_tracks(&pool).await.unwrap();

    RECONCILE_PHASE.store(PHASE_CANCELLED, Ordering::SeqCst);
    let report = reconcile(&pool, scan.hashed, scan.unreadable, remotes, true)
        .await
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.auto_linked, 0);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_track_link")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        rows, 0,
        "a cancel during reconcile must roll back before commit"
    );
    RECONCILE_PHASE.store(PHASE_IDLE, Ordering::SeqCst);
}

/// Build the fixture from the REAL compiled-in profile migrations rather
/// than a hand-rolled subset. A trimmed schema drops CHECK constraints,
/// NOT NULL columns and the HLC quartet the migrator adds, so a green test
/// on a fake schema proves nothing about production (cf. the DB-test
/// fidelity rule). `foreign_keys(true)` mirrors the runtime pool, and a
/// single connection keeps the `:memory:` database shared across queries.
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
    // Every `track` row FKs `library(id)`, so seed one library up front.
    sqlx::query("INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'Test', 0, 0)")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

async fn insert_local(pool: &SqlitePool, id: i64, path: &Path, title: &str) {
    sqlx::query(
        "INSERT INTO track
                (id, library_id, file_path, file_hash, file_size, file_modified,
                 title, duration_ms, added_at, is_available)
             VALUES (?, 1, ?, ?, ?, 0, ?, 0, 0, 1)",
    )
    .bind(id)
    .bind(path.to_string_lossy().as_ref())
    // A distinct partial-scan digest per row; deliberately never equal to
    // the full-file BLAKE3 the reconciler compares on.
    .bind(format!("localscan-{id}"))
    .bind(fs::metadata(path).unwrap().len() as i64)
    .bind(title)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_remote(pool: &SqlitePool, id: &str, bytes: &[u8], title: &str) {
    let hash = blake3::hash(bytes).to_hex().to_string();
    sqlx::query(
        "INSERT INTO remote_track (remote_id, title, size, full_hash, cached_at)
             VALUES (?, ?, ?, ?, 0)",
    )
    .bind(id)
    .bind(title)
    .bind(bytes.len() as i64)
    .bind(hash)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn unique_exact_content_is_linked_automatically() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.flac");
    fs::write(&path, b"identical audio bytes").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    insert_remote(&pool, "remote-1", b"identical audio bytes", "Remote").await;

    let report = discover(&pool).await.unwrap();
    assert_eq!(report.auto_linked, 1);
    assert!(report.candidates.is_empty());
    let links = links(&pool).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].local_track_id, 1);
    assert_eq!(links[0].remote_track_id, "remote-1");
    assert_eq!(links[0].playback_preference, "local_first");
}

#[tokio::test]
async fn equal_size_with_different_content_never_links() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.mp3");
    fs::write(&path, b"aaaa").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    insert_remote(&pool, "remote-1", b"bbbb", "Remote").await;

    let report = discover(&pool).await.unwrap();
    assert_eq!(report.hashed_local_tracks, 1);
    assert_eq!(report.auto_linked, 0);
    assert!(report.candidates.is_empty());
    assert!(links(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn user_data_copies_only_on_explicit_directional_actions() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.flac");
    fs::write(&path, b"same bytes").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    insert_remote(&pool, "remote-1", b"same bytes", "Remote").await;
    assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_mutation")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(queued, 0, "linking must not copy user data");

    sqlx::raw_sql(
        "INSERT INTO liked_track (track_id, liked_at) VALUES (1, 10);
             UPDATE track SET rating = 204 WHERE id = 1;
             INSERT INTO play_event (id, track_id, played_at, listened_ms)
                 VALUES (1, 1, 100, 0), (2, 1, 200, 0);
             INSERT INTO remote_history (track_remote_id, played_at, submission)
                 VALUES ('remote-1', 300, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();
    let link = links(&pool).await.unwrap().remove(0);
    assert!(link.local_favorite);
    assert!(!link.remote_favorite);
    assert_eq!(link.local_rating, Some(204));
    assert_eq!(link.remote_rating, None);
    assert_eq!(
        (link.local_plays, link.remote_plays, link.combined_plays),
        (2, 1, 3)
    );

    copy_favorite(&pool, 1, "local_to_server").await.unwrap();
    copy_rating(&pool, 1, "local_to_server").await.unwrap();
    let remote_rating: i64 = sqlx::query_scalar(
        "SELECT rating FROM remote_rating WHERE entity_type = 'track' AND entity_id = 'remote-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remote_rating, 4);

    sqlx::query("DELETE FROM liked_track WHERE track_id = 1")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE track SET rating = NULL WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    copy_favorite(&pool, 1, "server_to_local").await.unwrap();
    copy_rating(&pool, 1, "server_to_local").await.unwrap();
    let local: (i64, i64) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM liked_track WHERE track_id = 1), rating FROM track WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(local, (1, 204));
}

#[tokio::test]
async fn duplicate_content_requires_confirmation() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.wav");
    let second = dir.path().join("second.wav");
    fs::write(&first, b"same").unwrap();
    fs::write(&second, b"same").unwrap();
    insert_local(&pool, 1, &first, "First").await;
    insert_local(&pool, 2, &second, "Second").await;
    insert_remote(&pool, "remote-1", b"same", "Remote").await;

    let report = discover(&pool).await.unwrap();
    assert_eq!(report.auto_linked, 0);
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].local_tracks.len(), 2);

    confirm_exact(&pool, 2, "remote-1").await.unwrap();
    let links = links(&pool).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].local_track_id, 2);
}

#[tokio::test]
async fn duplicate_remote_copies_never_auto_link() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.flac");
    fs::write(&path, b"same").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    insert_remote(&pool, "remote-1", b"same", "First remote").await;
    insert_remote(&pool, "remote-2", b"same", "Second remote").await;

    let report = discover(&pool).await.unwrap();
    assert_eq!(report.auto_linked, 0);
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].remote_tracks.len(), 2);
    assert!(links(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn rejection_hides_the_same_proof() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.aac");
    let second = dir.path().join("second.aac");
    fs::write(&first, b"same").unwrap();
    fs::write(&second, b"same").unwrap();
    insert_local(&pool, 1, &first, "First").await;
    insert_local(&pool, 2, &second, "Second").await;
    insert_remote(&pool, "remote-1", b"same", "Remote").await;

    reject_exact(&pool, 1, "remote-1").await.unwrap();
    reject_exact(&pool, 2, "remote-1").await.unwrap();
    let report = discover(&pool).await.unwrap();
    assert_eq!(report.rejected_pairs, 2);
    assert!(report.candidates.is_empty());
    assert!(links(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn changed_bytes_mark_a_confirmed_link_stale() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.ogg");
    fs::write(&path, b"aaaa").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    insert_remote(&pool, "remote-1", b"aaaa", "Remote").await;
    assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);

    fs::write(&path, b"bbbb").unwrap();
    let report = discover(&pool).await.unwrap();
    assert_eq!(report.stale_links, 1);
    assert_eq!(links(&pool).await.unwrap()[0].status, "stale");
}

/// A linked track is re-read even when the digest cache holds an entry that
/// still looks valid.
///
/// The sweep discovers *and* re-verifies, and only the first may be served
/// from the cache. `track.file_modified` moves when a scan re-reads a file,
/// not when the file changes, so a track edited outside WaveFlow keeps its
/// `(size, mtime)` — and with them a cached digest describing bytes that
/// are gone. Serving a re-verification from that is how a stale link would
/// never be caught.
#[tokio::test]
async fn a_linked_track_is_re_read_even_when_cached() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.ogg");
    fs::write(&path, b"aaaa").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    insert_remote(&pool, "remote-1", b"aaaa", "Remote").await;
    assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);

    // The cache now holds the digest of "aaaa" against (4, 0). Rewriting to
    // the same length leaves both unchanged, so the entry still reads as
    // valid — and the link must go stale regardless.
    fs::write(&path, b"bbbb").unwrap();
    let cached = super::super::hashing::cached_digests(&pool, &[1])
        .await
        .unwrap();
    assert!(
        cached.contains_key(&1),
        "the fixture must leave a still-valid entry, or this proves nothing"
    );

    let report = discover(&pool).await.unwrap();
    assert_eq!(report.stale_links, 1);
}

#[tokio::test]
async fn a_path_move_keeps_and_reverifies_the_link() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("before.flac");
    let moved = dir.path().join("after.flac");
    fs::write(&first, b"same bytes").unwrap();
    insert_local(&pool, 1, &first, "Local").await;
    insert_remote(&pool, "remote-1", b"same bytes", "Remote").await;
    assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);

    fs::rename(&first, &moved).unwrap();
    sqlx::query("UPDATE track SET file_path = ? WHERE id = 1")
        .bind(moved.to_string_lossy().as_ref())
        .execute(&pool)
        .await
        .unwrap();
    let report = discover(&pool).await.unwrap();
    assert_eq!(report.verified_links, 1);
    assert_eq!(links(&pool).await.unwrap()[0].status, "confirmed");
}

#[tokio::test]
async fn playback_uses_only_confirmed_available_local_first_links() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.flac");
    fs::write(&path, b"same bytes").unwrap();
    insert_local(&pool, 1, &path, "Local").await;
    sqlx::query("UPDATE track SET duration_ms = 12_345 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    insert_remote(&pool, "remote-1", b"same bytes", "Remote").await;
    assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);

    let selected = preferred_local_playback(&pool, "remote-1")
        .await
        .unwrap()
        .expect("confirmed local-first link selected");
    assert_eq!(selected.track_id, 1);
    assert_eq!(selected.path, path);
    assert_eq!(selected.duration_ms, 12_345);

    set_playback_preference(&pool, 1, "server_first")
        .await
        .unwrap();
    assert!(preferred_local_playback(&pool, "remote-1")
        .await
        .unwrap()
        .is_none());

    set_playback_preference(&pool, 1, "local_first")
        .await
        .unwrap();
    sqlx::query("UPDATE track SET is_available = 0 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert!(preferred_local_playback(&pool, "remote-1")
        .await
        .unwrap()
        .is_none());

    sqlx::query("UPDATE track SET is_available = 1 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    fs::remove_file(&path).unwrap();
    assert!(preferred_local_playback(&pool, "remote-1")
        .await
        .unwrap()
        .is_none());

    fs::write(&path, b"same bytes").unwrap();
    sqlx::query("UPDATE remote_track_link SET status = 'stale' WHERE local_track_id = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert!(preferred_local_playback(&pool, "remote-1")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn local_playlist_conversion_blocks_until_every_track_is_linked() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.flac");
    let second = dir.path().join("second.mp3");
    fs::write(&first, b"first bytes").unwrap();
    fs::write(&second, b"second bytes").unwrap();
    insert_local(&pool, 1, &first, "First").await;
    insert_local(&pool, 2, &second, "Second").await;
    insert_remote(&pool, "remote-1", b"first bytes", "First remote").await;
    assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);
    sqlx::raw_sql(
        "INSERT INTO playlist (id, name, created_at, updated_at) VALUES (10, 'Local mix', 1, 1);
             INSERT INTO playlist_track (playlist_id, track_id, position, added_at)
                 VALUES (10, 1, 0, 1), (10, 2, 1, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let blocked = preview_playlist_conversion(&pool, "local_to_server", "10")
        .await
        .unwrap();
    assert!(!blocked.can_convert);
    assert_eq!(blocked.convertible_tracks, 1);
    assert_eq!(blocked.items[1].status, "unlinked_or_ambiguous");
    assert!(convert_playlist(&pool, "local_to_server", "10")
        .await
        .is_err());

    fs::write(&first, b"other bytes").unwrap();
    assert_eq!(discover(&pool).await.unwrap().stale_links, 1);
    let stale = preview_playlist_conversion(&pool, "local_to_server", "10")
        .await
        .unwrap();
    assert!(!stale.can_convert);
    assert_eq!(stale.items[0].status, "stale");

    fs::write(&first, b"first bytes").unwrap();
    assert_eq!(discover(&pool).await.unwrap().stale_links, 0);

    insert_remote(&pool, "remote-2", b"second bytes", "Second remote").await;
    assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);

    sqlx::query("UPDATE track SET is_available = 0 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let unavailable = preview_playlist_conversion(&pool, "local_to_server", "10")
        .await
        .unwrap();
    assert!(!unavailable.can_convert);
    assert_eq!(unavailable.items[0].status, "unlinked_or_ambiguous");
    assert!(convert_playlist(&pool, "local_to_server", "10")
        .await
        .is_err());
    sqlx::query("UPDATE track SET is_available = 1 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();

    let ready = preview_playlist_conversion(&pool, "local_to_server", "10")
        .await
        .unwrap();
    assert!(ready.can_convert);

    fs::write(&first, b"other bytes").unwrap();
    let changed_without_discover = preview_playlist_conversion(&pool, "local_to_server", "10")
        .await
        .unwrap();
    assert!(!changed_without_discover.can_convert);
    assert_eq!(changed_without_discover.items[0].status, "stale");
    assert!(convert_playlist(&pool, "local_to_server", "10")
        .await
        .is_err());
    fs::write(&first, b"first bytes").unwrap();

    let raced = convert_playlist_with_post_validation(&pool, "local_to_server", "10", || {
        fs::write(&first, b"other bytes").unwrap();
    })
    .await;
    assert!(raced.is_err());
    let rolled_back: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_playlist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rolled_back, 0, "the raced destination must roll back");
    fs::write(&first, b"first bytes").unwrap();

    let result = convert_playlist(&pool, "local_to_server", "10")
        .await
        .unwrap();
    assert_eq!(result.converted_tracks, 2);
    let copied: Vec<String> = sqlx::query_scalar(
        "SELECT track_remote_id FROM remote_playlist_track
              WHERE playlist_remote_id = ? ORDER BY position",
    )
    .bind(&result.destination_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(copied, vec!["remote-1", "remote-2"]);
    let mutations: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_mutation")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mutations, 1);
}

#[tokio::test]
async fn server_playlist_conversion_preserves_order_and_rejects_duplicates() {
    let pool = pool().await;
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.wav");
    let second = dir.path().join("second.aac");
    fs::write(&first, b"first bytes").unwrap();
    fs::write(&second, b"second bytes").unwrap();
    insert_local(&pool, 1, &first, "First").await;
    insert_local(&pool, 2, &second, "Second").await;
    insert_remote(&pool, "remote-1", b"first bytes", "First remote").await;
    insert_remote(&pool, "remote-2", b"second bytes", "Second remote").await;
    assert_eq!(discover(&pool).await.unwrap().auto_linked, 2);
    sqlx::raw_sql(
        "INSERT INTO remote_playlist (remote_id, name) VALUES ('valid', 'Server mix');
             INSERT INTO remote_playlist_track VALUES
                 ('valid', 0, 'remote-2'), ('valid', 1, 'remote-1');
             INSERT INTO remote_playlist (remote_id, name) VALUES ('duplicate', 'Duplicate mix');
             INSERT INTO remote_playlist_track VALUES
                 ('duplicate', 0, 'remote-1'), ('duplicate', 1, 'remote-1');",
    )
    .execute(&pool)
    .await
    .unwrap();

    let duplicate = preview_playlist_conversion(&pool, "server_to_local", "duplicate")
        .await
        .unwrap();
    assert!(!duplicate.can_convert);
    assert_eq!(duplicate.items[1].status, "duplicate");

    fs::write(&first, b"other bytes").unwrap();
    let changed_without_discover = preview_playlist_conversion(&pool, "server_to_local", "valid")
        .await
        .unwrap();
    assert!(!changed_without_discover.can_convert);
    assert_eq!(changed_without_discover.items[1].status, "stale");
    assert!(convert_playlist(&pool, "server_to_local", "valid")
        .await
        .is_err());
    fs::write(&first, b"first bytes").unwrap();

    sqlx::query("UPDATE track SET is_available = 0 WHERE id = 2")
        .execute(&pool)
        .await
        .unwrap();
    let unavailable = preview_playlist_conversion(&pool, "server_to_local", "valid")
        .await
        .unwrap();
    assert!(!unavailable.can_convert);
    assert_eq!(unavailable.items[0].status, "unlinked_or_ambiguous");
    assert!(convert_playlist(&pool, "server_to_local", "valid")
        .await
        .is_err());
    let local_playlists: i64 = sqlx::query_scalar("SELECT count(*) FROM playlist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(local_playlists, 0, "a blocked copy must remain atomic");
    sqlx::query("UPDATE track SET is_available = 1 WHERE id = 2")
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("DELETE FROM remote_track WHERE remote_id = 'remote-1'")
        .execute(&pool)
        .await
        .unwrap();
    let inaccessible = preview_playlist_conversion(&pool, "server_to_local", "valid")
        .await
        .unwrap();
    assert!(!inaccessible.can_convert);
    assert_eq!(inaccessible.items[1].status, "unlinked_or_ambiguous");
    insert_remote(&pool, "remote-1", b"first bytes", "First remote").await;

    let raced = convert_playlist_with_post_validation(&pool, "server_to_local", "valid", || {
        fs::write(&first, b"other bytes").unwrap();
    })
    .await;
    assert!(raced.is_err());
    let rolled_back: i64 = sqlx::query_scalar("SELECT count(*) FROM playlist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rolled_back, 0, "the raced destination must roll back");
    fs::write(&first, b"first bytes").unwrap();

    let result = convert_playlist(&pool, "server_to_local", "valid")
        .await
        .unwrap();
    let playlist_id = result.destination_id.parse::<i64>().unwrap();
    let copied: Vec<i64> = sqlx::query_scalar(
        "SELECT track_id FROM playlist_track WHERE playlist_id = ? ORDER BY position",
    )
    .bind(playlist_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(copied, vec![2, 1]);
}
