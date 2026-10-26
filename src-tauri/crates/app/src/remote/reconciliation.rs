//! Conservative local/server reconciliation (server RFC-004, M5).
//!
//! The server publishes a plain full-file BLAKE3 digest. The local
//! `track.file_hash` is deliberately a different, partial scan digest, so this
//! module first joins on byte size and only then reads the few possible local
//! matches in full. A link is automatic only when the exact digest identifies
//! one local row and one remote row. Every multiplicity stays a candidate for
//! a human decision.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

use chrono::Utc;
use serde::Serialize;
use sqlx::{Row, SqliteConnection, SqlitePool};
use tauri::{AppHandle, Emitter};

use waveflow_core::repository::{
    playlist::PlaylistDraft,
    sqlite::playlist::{append_tracks_conn, insert_custom_conn},
};

use crate::error::{AppError, AppResult};

const STATUS_CONFIRMED: &str = "confirmed";
const STATUS_STALE: &str = "stale";

/// Reconciliation-scan lifecycle as a single atomic state machine so the
/// cancel-vs-commit race resolves with one compare-exchange instead of two
/// separate flags. The scan reads and full-hashes local files, so it can run
/// for a while on a large library even behind the byte-size prefilter.
///
/// Transitions (all `compare_exchange`, so exactly one racer wins):
/// - `IDLE → STAGING`: claim the slot ([`discover_with_progress`]); any other
///   current value means a scan is already running.
/// - `STAGING → CANCELLED`: a cancel wins ([`request_cancel`]); only possible
///   while staging, never once the commit has begun.
/// - `STAGING → COMMITTING`: the run wins the race and starts persisting
///   ([`reconcile`]); a cancel can no longer take effect.
/// - `* → IDLE`: the [`PhaseGuard`] resets on every exit path.
///
/// Because the same `STAGING → {CANCELLED, COMMITTING}` compare-exchange
/// arbitrates both sides, `request_cancel` can never report success after
/// persistence has started, and the commit can never run after a cancel won.
const PHASE_IDLE: u8 = 0;
const PHASE_STAGING: u8 = 1;
const PHASE_CANCELLED: u8 = 2;
const PHASE_COMMITTING: u8 = 3;
static RECONCILE_PHASE: AtomicU8 = AtomicU8::new(PHASE_IDLE);

/// Progress payload for the `reconcile:progress` event the UI drives its bar
/// from. `processed` counts local files whose full hash has been computed.
#[derive(Debug, Clone, Serialize)]
struct ReconcileProgress {
    processed: usize,
    total: usize,
}

/// Outcome of the blocking hash pass over the local candidates.
struct HashScan {
    hashed: Vec<HashedLocalTrack>,
    /// `(track id, digest, size, mtime)` for the files this sweep actually
    /// read, so the caller can file them for the next one. Empty when every
    /// candidate was already known.
    computed: Vec<(i64, String, i64, i64)>,
    unreadable: usize,
    cancelled: bool,
}

/// RAII guard resetting [`RECONCILE_PHASE`] to `IDLE` on every exit path (early
/// return, `?`, panic) so a failed scan can't wedge the state and brick the
/// feature for the session.
struct PhaseGuard;

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        RECONCILE_PHASE.store(PHASE_IDLE, Ordering::SeqCst);
    }
}

/// Ask an in-flight reconciliation scan to stop. Succeeds only while the scan
/// is still staging (hashing / building links); once the commit has begun it is
/// too late and this returns `false`, so a `true` return is an authoritative
/// promise that nothing was persisted. Idempotent and safe to call when nothing
/// is running.
pub fn request_cancel() -> bool {
    match RECONCILE_PHASE.compare_exchange(
        PHASE_STAGING,
        PHASE_CANCELLED,
        Ordering::SeqCst,
        Ordering::SeqCst,
    ) {
        Ok(_) => true,
        // Already cancelled (double click) — idempotently still "cancelled".
        Err(PHASE_CANCELLED) => true,
        // IDLE (nothing running) or COMMITTING (too late to stop).
        Err(_) => false,
    }
}

#[derive(Debug, Clone)]
struct LocalTrack {
    id: i64,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    file_path: String,
    size: i64,
    /// Carried so a computed digest can be filed against the same
    /// `(size, mtime)` the row had when it was read — see [`super::hashing`].
    modified: i64,
}

#[derive(Debug, Clone)]
struct HashedLocalTrack {
    track: LocalTrack,
    full_hash: String,
}

#[derive(Debug, Clone)]
struct RemoteTrack {
    id: String,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    size: i64,
    full_hash: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalMatchCandidate {
    pub track_id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub file_path: String,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteMatchCandidate {
    pub track_id: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MatchCandidateGroup {
    pub full_hash: String,
    pub local_tracks: Vec<LocalMatchCandidate>,
    pub remote_tracks: Vec<RemoteMatchCandidate>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub hashed_local_tracks: usize,
    pub unreadable_local_tracks: usize,
    pub auto_linked: usize,
    pub verified_links: usize,
    pub stale_links: usize,
    pub rejected_pairs: usize,
    /// `true` when the user cancelled mid-scan. A cancelled run persists
    /// nothing new and returns an otherwise-empty report so the UI can render
    /// "Cancelled" instead of a misleading zero-match result.
    pub cancelled: bool,
    /// `true` when another scan already owns the run (e.g. a double-clicked
    /// "Find matches"). Distinct from a genuinely empty result: the UI must
    /// ignore this report and keep whatever candidates it already shows, rather
    /// than clearing them to a false "0 matches".
    pub already_running: bool,
    pub candidates: Vec<MatchCandidateGroup>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReconciliationLink {
    pub local_track_id: i64,
    pub remote_track_id: String,
    pub local_title: String,
    pub remote_title: Option<String>,
    pub method: String,
    pub verified_full_hash: Option<String>,
    pub status: String,
    pub playback_preference: String,
    pub confirmed_at: i64,
    pub verified_at: i64,
    pub local_favorite: bool,
    pub remote_favorite: bool,
    pub local_rating: Option<i64>,
    pub remote_rating: Option<i64>,
    pub local_plays: i64,
    pub remote_plays: i64,
    pub combined_plays: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferredLocalPlayback {
    pub track_id: i64,
    pub path: PathBuf,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlaylistConversionItem {
    pub position: i64,
    pub title: String,
    pub local_track_id: Option<i64>,
    pub remote_track_id: Option<String>,
    /// `confirmed`, `stale`, `unlinked_or_ambiguous`, or `duplicate`.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlaylistConversionPreview {
    pub direction: String,
    pub source_id: String,
    pub source_name: String,
    pub total_tracks: usize,
    pub convertible_tracks: usize,
    pub blocked_tracks: usize,
    pub can_convert: bool,
    pub items: Vec<PlaylistConversionItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlaylistConversionResult {
    pub direction: String,
    pub destination_id: String,
    pub converted_tracks: usize,
}

#[derive(Debug, Clone)]
struct ExistingLink {
    local_track_id: i64,
    remote_track_id: String,
    verified_full_hash: Option<String>,
}

#[derive(Debug)]
struct PlaylistLinkProof {
    local_track_id: i64,
    file_path: String,
    verified_full_hash: Option<String>,
    remote_full_hash: Option<String>,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn valid_full_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn empty_report(cancelled: bool) -> ReconciliationReport {
    ReconciliationReport {
        hashed_local_tracks: 0,
        unreadable_local_tracks: 0,
        auto_linked: 0,
        verified_links: 0,
        stale_links: 0,
        rejected_pairs: 0,
        cancelled,
        already_running: false,
        candidates: Vec::new(),
    }
}

/// Atomically leave the cancellable `STAGING` phase at a terminal / commit
/// point. Returns `Some(cancelled_report)` when a cancel already won the
/// `STAGING → CANCELLED` race and the caller must bail without persisting;
/// `None` to proceed. On the non-cancellable path it is always `None`. Using
/// this at every terminal point — the empty-remote early return and the
/// pre-commit gate in [`reconcile`] alike — keeps `request_cancel`'s
/// authoritative `true` consistent with the report's `cancelled` flag.
fn take_commit_slot(cancellable: bool) -> Option<ReconciliationReport> {
    if cancellable
        && RECONCILE_PHASE
            .compare_exchange(
                PHASE_STAGING,
                PHASE_COMMITTING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
    {
        return Some(empty_report(true));
    }
    None
}

/// Test-only convenience entry: run the discovery pipeline with no progress
/// events and no cancellation. Production always goes through
/// [`discover_with_progress`].
#[cfg(test)]
async fn discover(pool: &SqlitePool) -> AppResult<ReconciliationReport> {
    discover_inner(pool, None, false).await
}

/// Same as [`discover`], but emits `reconcile:progress` events for the UI and
/// honours the shared cancel flag. Overlapping scans are rejected up front so
/// two `RUNNING` runs can't interleave their progress and cancel state.
pub async fn discover_with_progress(
    pool: &SqlitePool,
    app: AppHandle,
) -> AppResult<ReconciliationReport> {
    // Claim the scan slot atomically: only `IDLE → STAGING` succeeds, so a
    // second concurrent scan (any non-IDLE state) bails with an empty,
    // non-cancelled report rather than a scary error toast.
    if RECONCILE_PHASE
        .compare_exchange(
            PHASE_IDLE,
            PHASE_STAGING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        // A scan already owns the run. Signal that explicitly so the UI keeps
        // its current candidates instead of clearing them to a false "0
        // matches"; this is NOT the same as a genuinely empty remote library.
        return Ok(ReconciliationReport {
            already_running: true,
            ..empty_report(false)
        });
    }
    // The guard resets the phase to IDLE on every exit path (early return, `?`,
    // panic), whether we ended in STAGING, CANCELLED or COMMITTING.
    let _guard = PhaseGuard;
    // The progress path is cancellable and emits through `app`.
    discover_inner(pool, Some(app), true).await
}

/// Shared discovery pipeline. `cancellable` is passed explicitly rather than
/// inferred from `app`, so a unit test can exercise the cancellation path with
/// no `AppHandle` (a `None` progress sink) — and so the non-cancellable path
/// provably never reads or transitions the process-global phase.
async fn discover_inner(
    pool: &SqlitePool,
    app: Option<AppHandle>,
    cancellable: bool,
) -> AppResult<ReconciliationReport> {
    // Only the cancellable path announces itself: the other one is a
    // synchronous helper with no user-visible duration, and a row that
    // appears and vanishes within a frame is noise (#601). Routed to
    // the existing `request_cancel`, which knows the scan may only stop
    // between batches.
    let task = app.as_ref().filter(|_| cancellable).and_then(|handle| {
        crate::tasks::start(
            handle,
            crate::tasks::TaskKind::Reconcile,
            0,
            crate::tasks::cancel_fn(|| {
                request_cancel();
            }),
        )
        // Behind an `Arc` because the hashing pass runs on the blocking
        // pool and needs an owned, `'static` reference to report into.
        // The row still retires when the last clone drops, which is the
        // end of this function either way.
        .map(std::sync::Arc::new)
    });

    let remote_tracks = load_remote_tracks(pool).await?;
    if remote_tracks.is_empty() {
        // Nothing to reconcile, but this is still a terminal point: arbitrate
        // the phase exactly like `reconcile` so a cancel that raced this path is
        // reported consistently instead of as a plain empty result.
        return Ok(take_commit_slot(cancellable).unwrap_or_else(|| empty_report(false)));
    }

    let local_tracks = load_local_candidates(pool).await?;
    let total = local_tracks.len();
    // What a previous sweep — or an upload survey, or an import — already
    // established about these exact files. Read once, before the blocking pass,
    // because that pass has no pool and must not acquire one per file.
    //
    // **Only for tracks that carry no link yet.** This sweep does two jobs, and
    // they are the two the digest cache exists to keep apart: it *discovers*
    // pairs among unlinked tracks, and it *re-verifies* the tracks that already
    // have a link, which is how a link whose bytes changed becomes `stale`.
    // Serving that second job from the cache would defeat it — `file_modified`
    // only moves when a scan re-reads the file, so a track edited outside
    // WaveFlow keeps its `(size, mtime)` and its cached digest, and its stale
    // link would never be caught. A linked track is therefore always read.
    let unlinked: Vec<i64> = sqlx::query_scalar(
        "SELECT t.id FROM track t
          WHERE NOT EXISTS (SELECT 1 FROM remote_track_link l WHERE l.local_track_id = t.id)",
    )
    .fetch_all(pool)
    .await?;
    let unlinked: std::collections::HashSet<i64> = unlinked.into_iter().collect();
    let ids: Vec<i64> = local_tracks
        .iter()
        .map(|track| track.id)
        .filter(|id| unlinked.contains(id))
        .collect();
    let known = super::hashing::cached_digests(pool, &ids).await?;
    let hash_task = task.clone();
    let scan = tokio::task::spawn_blocking(move || {
        hash_local_tracks(
            local_tracks,
            known,
            app.as_ref(),
            hash_task.as_deref(),
            total,
            cancellable,
        )
    })
    .await
    .map_err(|err| AppError::Other(format!("reconciliation hash task failed: {err}")))?;
    // Filed before the cancellation check below: these describe files, not
    // decisions, so a run that was stopped still leaves the reading it paid for
    // behind — which is the whole reason the next sweep is cheap. Failing to
    // file them must not fail the reconciliation: the digests are an
    // optimisation for the next run, and this run's answer is already in hand.
    super::hashing::remember_quietly(pool, &scan.computed).await;

    if scan.cancelled {
        // A partial scan could auto-link the unique matches it happened to
        // reach and leave the rest looking "resolved"; a cancelled run must
        // persist nothing, so bail before touching the database.
        return Ok(empty_report(true));
    }

    reconcile(
        pool,
        scan.hashed,
        scan.unreadable,
        remote_tracks,
        cancellable,
    )
    .await
}

async fn load_remote_tracks(pool: &SqlitePool) -> AppResult<Vec<RemoteTrack>> {
    let rows = sqlx::query(
        "SELECT remote_id, title, artist, album, size, full_hash
           FROM remote_track
          WHERE size IS NOT NULL AND size >= 0 AND full_hash IS NOT NULL
          ORDER BY remote_id",
    )
    .fetch_all(pool)
    .await?;

    let mut tracks = Vec::with_capacity(rows.len());
    for row in rows {
        let full_hash: String = row.try_get("full_hash")?;
        if !valid_full_hash(&full_hash) {
            continue;
        }
        tracks.push(RemoteTrack {
            id: row.try_get("remote_id")?,
            title: row.try_get("title")?,
            artist: row.try_get("artist")?,
            album: row.try_get("album")?,
            size: row.try_get("size")?,
            full_hash: full_hash.to_ascii_lowercase(),
        });
    }
    Ok(tracks)
}

async fn load_local_candidates(pool: &SqlitePool) -> AppResult<Vec<LocalTrack>> {
    let rows = sqlx::query(
        "SELECT t.id, t.title, t.file_path, t.file_size, t.file_modified, al.title AS album,
                (SELECT GROUP_CONCAT(name, ', ') FROM (
                    SELECT ar.name
                      FROM track_artist ta
                      JOIN artist ar ON ar.id = ta.artist_id
                     WHERE ta.track_id = t.id
                     ORDER BY ta.position
                )) AS artist
           FROM track t
           LEFT JOIN album al ON al.id = t.album_id
          WHERE t.is_available = 1
            AND (EXISTS (SELECT 1 FROM remote_track rt
                          WHERE rt.size = t.file_size AND rt.size >= 0
                            AND rt.full_hash IS NOT NULL)
                 OR EXISTS (SELECT 1 FROM remote_track_link l
                             WHERE l.local_track_id = t.id))
          ORDER BY t.id",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(LocalTrack {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                artist: row.try_get("artist")?,
                album: row.try_get("album")?,
                file_path: row.try_get("file_path")?,
                size: row.try_get("file_size")?,
                modified: row.try_get("file_modified")?,
            })
        })
        .collect()
}

fn hash_local_tracks(
    tracks: Vec<LocalTrack>,
    known: HashMap<i64, String>,
    app: Option<&AppHandle>,
    task: Option<&crate::tasks::TaskHandle>,
    total: usize,
    cancellable: bool,
) -> HashScan {
    // The phase is only meaningful on the cancellable path; the no-progress/test
    // path must not read the shared phase (see `discover_inner`).
    let mut hashed = Vec::with_capacity(tracks.len());
    let mut computed: Vec<(i64, String, i64, i64)> = Vec::new();
    let mut unreadable = 0;
    for track in tracks {
        // Poll for a cancel at the top of the loop so a click that lands
        // between two files is honoured before starting another full-file read.
        if cancellable && RECONCILE_PHASE.load(Ordering::Relaxed) == PHASE_CANCELLED {
            return cancelled_scan(hashed, computed, unreadable, total);
        }
        // Already known for this exact `(size, mtime)`: reading the file again
        // would cost a full pass over the library to learn what the last one
        // learned. Discovery wants to know the digest, not to re-check the
        // bytes — the callers that need that use `hashing::verify`.
        if let Some(full_hash) = known.get(&track.id).cloned() {
            hashed.push(HashedLocalTrack { track, full_hash });
            if let Some(app) = app {
                let processed = hashed.len() + unreadable;
                let _ = app.emit("reconcile:progress", ReconcileProgress { processed, total });
            }
            // Outside the `app` guard: the status bar is fed by the
            // registry, not by the event, so a run with no `AppHandle`
            // (the test path) must not be the only thing deciding
            // whether progress moves.
            if let Some(task) = task {
                task.progress((hashed.len() + unreadable) as u64, total as u64);
            }
            continue;
        }
        match waveflow_core::scanner::hash_file_full(Path::new(&track.file_path)) {
            Ok(full_hash) => {
                computed.push((track.id, full_hash.clone(), track.size, track.modified));
                hashed.push(HashedLocalTrack { track, full_hash });
            }
            Err(err) => {
                unreadable += 1;
                tracing::warn!(
                    local_track_id = track.id,
                    error = %err,
                    "reconciliation full-content hash failed; excluding track"
                );
            }
        }
        // `hashed + unreadable` is exactly the count of files processed so far.
        if let Some(app) = app {
            let processed = hashed.len() + unreadable;
            let _ = app.emit("reconcile:progress", ReconcileProgress { processed, total });
        }
        // The branch above returns early on a cached digest, so this is
        // the other half — the freshly hashed files, which are the slow
        // ones and therefore the ones the user is watching.
        if let Some(task) = task {
            task.progress((hashed.len() + unreadable) as u64, total as u64);
        }
    }
    // A cancel arriving while the LAST file hashes would never be seen by the
    // top-of-loop poll, so the run would persist despite the click. Re-check
    // once after the loop before declaring the scan complete. The definitive
    // arbitration still happens at the pre-commit compare-exchange in
    // `reconcile`, which catches a cancel this relaxed load may have missed.
    if cancellable && RECONCILE_PHASE.load(Ordering::Relaxed) == PHASE_CANCELLED {
        return cancelled_scan(hashed, computed, unreadable, total);
    }
    HashScan {
        hashed,
        computed,
        unreadable,
        cancelled: false,
    }
}

fn cancelled_scan(
    hashed: Vec<HashedLocalTrack>,
    computed: Vec<(i64, String, i64, i64)>,
    unreadable: usize,
    total: usize,
) -> HashScan {
    let processed = hashed.len() + unreadable;
    tracing::info!(processed, total, "reconciliation scan cancelled by user");
    // A cancelled sweep persists no *links* — that is the invariant — but the
    // digests it computed describe files, not decisions, and throwing them away
    // would make a resumed run re-read everything it already read.
    HashScan {
        hashed,
        computed,
        unreadable,
        cancelled: true,
    }
}

/// Re-read every local file behind a playlist conversion and say whether its
/// bytes still match the proof its link was written from.
///
/// Deliberately *not* served from [`super::hashing`]'s cache. This is a
/// verification, not a discovery: the caller is about to convert a playlist,
/// and the question is what the file contains now — an entry that is valid for
/// its `(size, mtime)` cannot answer that, since the one rewrite that pair
/// cannot see is exactly the one this guards against.
async fn playlist_link_freshness(proofs: Vec<PlaylistLinkProof>) -> AppResult<HashMap<i64, bool>> {
    tokio::task::spawn_blocking(move || {
        let mut freshness = HashMap::new();
        for proof in proofs {
            if freshness.contains_key(&proof.local_track_id) {
                continue;
            }
            let expected = proof
                .verified_full_hash
                .as_deref()
                .filter(|value| valid_full_hash(value));
            let remote = proof
                .remote_full_hash
                .as_deref()
                .filter(|value| valid_full_hash(value));
            let fresh = match (expected, remote) {
                (Some(expected), Some(remote)) if expected.eq_ignore_ascii_case(remote) => {
                    match waveflow_core::scanner::hash_file_full(Path::new(&proof.file_path)) {
                        Ok(current) => current.eq_ignore_ascii_case(expected),
                        Err(err) => {
                            tracing::warn!(
                                local_track_id = proof.local_track_id,
                                error = %err,
                                "playlist conversion could not revalidate local track"
                            );
                            false
                        }
                    }
                }
                _ => false,
            };
            freshness.insert(proof.local_track_id, fresh);
        }
        freshness
    })
    .await
    .map_err(|err| AppError::Other(format!("reconciliation hash task failed: {err}")))
}

async fn reconcile(
    pool: &SqlitePool,
    local_tracks: Vec<HashedLocalTrack>,
    unreadable_local_tracks: usize,
    remote_tracks: Vec<RemoteTrack>,
    cancellable: bool,
) -> AppResult<ReconciliationReport> {
    let hashed_local_count = local_tracks.len();
    let mut local_by_hash: HashMap<String, Vec<HashedLocalTrack>> = HashMap::new();
    let mut local_by_id = HashMap::new();
    for local in local_tracks {
        local_by_id.insert(local.track.id, local.clone());
        local_by_hash
            .entry(local.full_hash.clone())
            .or_default()
            .push(local);
    }

    let mut remote_by_hash: HashMap<String, Vec<RemoteTrack>> = HashMap::new();
    let mut remote_by_id = HashMap::new();
    for remote in remote_tracks {
        remote_by_id.insert(remote.id.clone(), remote.clone());
        remote_by_hash
            .entry(remote.full_hash.clone())
            .or_default()
            .push(remote);
    }

    let mut tx = pool.begin().await?;
    let link_rows = sqlx::query(
        "SELECT local_track_id, remote_track_id, verified_full_hash
           FROM remote_track_link",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut links = Vec::with_capacity(link_rows.len());
    for row in link_rows {
        links.push(ExistingLink {
            local_track_id: row.try_get("local_track_id")?,
            remote_track_id: row.try_get("remote_track_id")?,
            verified_full_hash: row.try_get("verified_full_hash")?,
        });
    }

    let rejection_rows = sqlx::query(
        "SELECT local_track_id, remote_track_id, proof
           FROM remote_track_match_rejection
          WHERE proof_kind = 'exact_full_hash'",
    )
    .fetch_all(&mut *tx)
    .await?;
    let rejections: HashSet<(i64, String, String)> = rejection_rows
        .into_iter()
        .map(|row| {
            Ok((
                row.try_get("local_track_id")?,
                row.try_get("remote_track_id")?,
                row.try_get("proof")?,
            ))
        })
        .collect::<Result<_, sqlx::Error>>()?;

    let now = now_ms();
    let mut verified_links = 0;
    let mut stale_links = 0;
    let mut linked_local: HashMap<i64, String> = HashMap::new();
    let mut linked_remote: HashMap<String, i64> = HashMap::new();

    for link in &links {
        linked_local.insert(link.local_track_id, link.remote_track_id.clone());
        linked_remote.insert(link.remote_track_id.clone(), link.local_track_id);

        let Some(local) = local_by_id.get(&link.local_track_id) else {
            // An unavailable local file keeps its link; availability is not
            // evidence that the identity changed.
            continue;
        };
        let Some(remote) = remote_by_id.get(&link.remote_track_id) else {
            // The remote cache is disposable and may be between refills.
            continue;
        };
        let matches = link.verified_full_hash.as_deref() == Some(local.full_hash.as_str())
            && local.full_hash == remote.full_hash;
        let status = if matches {
            verified_links += 1;
            STATUS_CONFIRMED
        } else {
            stale_links += 1;
            STATUS_STALE
        };
        sqlx::query(
            "UPDATE remote_track_link SET status = ?, verified_at = ?
              WHERE local_track_id = ?",
        )
        .bind(status)
        .bind(now)
        .bind(link.local_track_id)
        .execute(&mut *tx)
        .await?;
    }

    let mut auto_linked = 0;
    for (hash, locals) in &local_by_hash {
        let Some(remotes) = remote_by_hash.get(hash) else {
            continue;
        };
        if locals.len() != 1 || remotes.len() != 1 {
            continue;
        }
        let local = &locals[0];
        let remote = &remotes[0];
        if linked_local.contains_key(&local.track.id)
            || linked_remote.contains_key(&remote.id)
            || rejections.contains(&(local.track.id, remote.id.clone(), hash.clone()))
        {
            continue;
        }

        sqlx::query(
            "INSERT INTO remote_track_link
                (local_track_id, remote_track_id, method, verified_full_hash,
                 status, playback_preference, confirmed_at, verified_at)
             VALUES (?, ?, 'exact_full_hash', ?, 'confirmed', 'local_first', ?, ?)",
        )
        .bind(local.track.id)
        .bind(&remote.id)
        .bind(hash)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        linked_local.insert(local.track.id, remote.id.clone());
        linked_remote.insert(remote.id.clone(), local.track.id);
        auto_linked += 1;
    }

    let mut hashes: Vec<String> = local_by_hash
        .keys()
        .filter(|hash| remote_by_hash.contains_key(*hash))
        .cloned()
        .collect();
    hashes.sort();

    let mut candidates = Vec::new();
    let mut rejected_pairs = 0;
    for hash in hashes {
        let locals = &local_by_hash[&hash];
        let remotes = &remote_by_hash[&hash];
        let mut candidate_locals = Vec::new();
        let mut candidate_remotes = Vec::new();
        let mut has_unrejected_pair = false;

        for local in locals {
            if linked_local.contains_key(&local.track.id) {
                continue;
            }
            candidate_locals.push(local_candidate(local));
            for remote in remotes {
                if linked_remote.contains_key(&remote.id) {
                    continue;
                }
                if rejections.contains(&(local.track.id, remote.id.clone(), hash.clone())) {
                    rejected_pairs += 1;
                } else {
                    has_unrejected_pair = true;
                }
            }
        }
        for remote in remotes {
            if !linked_remote.contains_key(&remote.id) {
                candidate_remotes.push(remote_candidate(remote));
            }
        }

        if has_unrejected_pair && !candidate_locals.is_empty() && !candidate_remotes.is_empty() {
            candidates.push(MatchCandidateGroup {
                full_hash: hash,
                local_tracks: candidate_locals,
                remote_tracks: candidate_remotes,
            });
        }
    }

    // Atomically leave the cancellable phase right before persisting: only
    // `STAGING → COMMITTING` lets the commit through, and it fails exactly when
    // a `request_cancel` already won `STAGING → CANCELLED`. On failure we drop
    // `tx` (rollback) so a cancelled run persists nothing, and `request_cancel`
    // — having observed STAGING — returned an authoritative `true`. The
    // no-progress/test path (`cancellable == false`) never entered STAGING and
    // always commits. All writes are staged in `tx`, so rollback leaves nothing.
    if let Some(cancelled) = take_commit_slot(cancellable) {
        tracing::info!("reconciliation scan cancelled before commit; rolling back");
        return Ok(cancelled);
    }
    tx.commit().await?;
    Ok(ReconciliationReport {
        hashed_local_tracks: hashed_local_count,
        unreadable_local_tracks,
        auto_linked,
        verified_links,
        stale_links,
        rejected_pairs,
        cancelled: false,
        already_running: false,
        candidates,
    })
}

fn local_candidate(local: &HashedLocalTrack) -> LocalMatchCandidate {
    LocalMatchCandidate {
        track_id: local.track.id,
        title: local.track.title.clone(),
        artist: local.track.artist.clone(),
        album: local.track.album.clone(),
        file_path: local.track.file_path.clone(),
        size: local.track.size,
    }
}

fn remote_candidate(remote: &RemoteTrack) -> RemoteMatchCandidate {
    RemoteMatchCandidate {
        track_id: remote.id.clone(),
        title: remote.title.clone(),
        artist: remote.artist.clone(),
        album: remote.album.clone(),
        size: remote.size,
    }
}

/// Confirm one exact pair from an ambiguous group. The local file is re-read
/// immediately before the transaction so a stale UI cannot confirm old bytes.
pub async fn confirm_exact(
    pool: &SqlitePool,
    local_track_id: i64,
    remote_track_id: &str,
) -> AppResult<()> {
    let row = sqlx::query(
        "SELECT t.file_path, t.file_size, t.file_modified,
                rt.size AS remote_size, rt.full_hash
           FROM track t
           JOIN remote_track rt ON rt.remote_id = ?
          WHERE t.id = ? AND t.is_available = 1",
    )
    .bind(remote_track_id)
    .bind(local_track_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Other("local or remote track is unavailable".into()))?;

    let file_path: String = row.try_get("file_path")?;
    let local_size: i64 = row.try_get("file_size")?;
    let local_modified: i64 = row.try_get("file_modified")?;
    let remote_size: i64 = row.try_get("remote_size")?;
    let remote_hash: String = row.try_get("full_hash")?;
    if local_size != remote_size || !valid_full_hash(&remote_hash) {
        return Err(AppError::Other(
            "tracks do not share a valid exact-content candidate".into(),
        ));
    }

    // Reads the file, deliberately: this is a verification immediately before an
    // irreversible write, and the cache exists to avoid re-reading during
    // *discovery*. Going through `hashing::verify` rather than hashing directly
    // means the fresh reading also repairs whatever the cache held.
    let local_hash =
        super::hashing::verify(pool, local_track_id, &file_path, local_size, local_modified)
            .await?
            .ok_or_else(|| AppError::Other("local file could not be read".into()))?;
    if !local_hash.eq_ignore_ascii_case(&remote_hash) {
        return Err(AppError::Other("track contents no longer match".into()));
    }

    link_exact(pool, local_track_id, remote_track_id, &remote_hash).await
}

/// Write one exact link, in its own transaction.
///
/// Shared with the import path, which reaches this point having *just written*
/// the bytes and so already knowing the digest — the one case where an exact
/// link costs nothing. Keeping the write here rather than copying it is what
/// stops the two callers from drifting on the guard below: a link is refused
/// when either side is already linked elsewhere, because `remote_track_link`
/// is one-to-one in both directions and an `ON CONFLICT` on `local_track_id`
/// alone would silently steal a remote track from another local row.
pub(super) async fn link_exact(
    pool: &SqlitePool,
    local_track_id: i64,
    remote_track_id: &str,
    full_hash: &str,
) -> AppResult<()> {
    if !valid_full_hash(full_hash) {
        return Err(AppError::Other("link proof is not a full digest".into()));
    }
    let normalized_hash = full_hash.to_ascii_lowercase();
    let now = now_ms();
    let mut tx = pool.begin().await?;
    let conflict: Option<(i64, String)> = sqlx::query_as(
        "SELECT local_track_id, remote_track_id FROM remote_track_link
          WHERE (local_track_id = ? AND remote_track_id != ?)
             OR (remote_track_id = ? AND local_track_id != ?)",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(remote_track_id)
    .bind(local_track_id)
    .fetch_optional(&mut *tx)
    .await?;
    if conflict.is_some() {
        return Err(AppError::Other(
            "one of these tracks is already linked elsewhere".into(),
        ));
    }

    sqlx::query(
        "INSERT INTO remote_track_link
            (local_track_id, remote_track_id, method, verified_full_hash,
             status, playback_preference, confirmed_at, verified_at)
         VALUES (?, ?, 'exact_full_hash', ?, 'confirmed', 'local_first', ?, ?)
         ON CONFLICT(local_track_id) DO UPDATE SET
             remote_track_id = excluded.remote_track_id,
             method = excluded.method,
             verified_full_hash = excluded.verified_full_hash,
             status = excluded.status,
             verified_at = excluded.verified_at",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(&normalized_hash)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM remote_track_match_rejection
          WHERE local_track_id = ? AND remote_track_id = ?
            AND proof_kind = 'exact_full_hash' AND proof = ?",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(&normalized_hash)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Hide an exact candidate until its proof changes.
pub async fn reject_exact(
    pool: &SqlitePool,
    local_track_id: i64,
    remote_track_id: &str,
) -> AppResult<()> {
    let row = sqlx::query(
        "SELECT rt.full_hash,
                EXISTS(SELECT 1 FROM track t WHERE t.id = ?) AS local_exists,
                EXISTS(SELECT 1 FROM remote_track_link l
                        WHERE l.local_track_id = ? OR l.remote_track_id = ?) AS linked
           FROM remote_track rt WHERE rt.remote_id = ?",
    )
    .bind(local_track_id)
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Other("remote track is unavailable".into()))?;
    let local_exists = row.try_get::<i64, _>("local_exists")? != 0;
    let linked = row.try_get::<i64, _>("linked")? != 0;
    let proof: String = row.try_get("full_hash")?;
    if !local_exists || !valid_full_hash(&proof) {
        return Err(AppError::Other("candidate is unavailable".into()));
    }
    if linked {
        return Err(AppError::Other("linked tracks cannot be rejected".into()));
    }

    sqlx::query(
        "INSERT OR IGNORE INTO remote_track_match_rejection
            (local_track_id, remote_track_id, proof_kind, proof, rejected_at)
         VALUES (?, ?, 'exact_full_hash', ?, ?)",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(proof.to_ascii_lowercase())
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn links(pool: &SqlitePool) -> AppResult<Vec<ReconciliationLink>> {
    let rows = sqlx::query(
        "SELECT l.local_track_id, l.remote_track_id, t.title AS local_title,
                rt.title AS remote_title, l.method, l.verified_full_hash,
                l.status, l.playback_preference, l.confirmed_at, l.verified_at,
                EXISTS(SELECT 1 FROM liked_track f WHERE f.track_id = t.id) AS local_favorite,
                EXISTS(SELECT 1 FROM remote_favorite f
                        WHERE f.entity_type = 'track' AND f.entity_id = l.remote_track_id) AS remote_favorite,
                t.rating AS local_rating,
                (SELECT rating FROM remote_rating r
                  WHERE r.entity_type = 'track' AND r.entity_id = l.remote_track_id) AS remote_rating,
                (SELECT count(*) FROM play_event p WHERE p.track_id = t.id) AS local_plays,
                (SELECT count(*) FROM remote_history h
                  WHERE h.track_remote_id = l.remote_track_id) AS remote_plays
           FROM remote_track_link l
           JOIN track t ON t.id = l.local_track_id
           LEFT JOIN remote_track rt ON rt.remote_id = l.remote_track_id
          ORDER BY lower(t.title), l.local_track_id",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let local_plays = row.try_get("local_plays")?;
            let remote_plays = row.try_get("remote_plays")?;
            Ok(ReconciliationLink {
                local_track_id: row.try_get("local_track_id")?,
                remote_track_id: row.try_get("remote_track_id")?,
                local_title: row.try_get("local_title")?,
                remote_title: row.try_get("remote_title")?,
                method: row.try_get("method")?,
                verified_full_hash: row.try_get("verified_full_hash")?,
                status: row.try_get("status")?,
                playback_preference: row.try_get("playback_preference")?,
                confirmed_at: row.try_get("confirmed_at")?,
                verified_at: row.try_get("verified_at")?,
                local_favorite: row.try_get::<i64, _>("local_favorite")? != 0,
                remote_favorite: row.try_get::<i64, _>("remote_favorite")? != 0,
                local_rating: row.try_get("local_rating")?,
                remote_rating: row.try_get("remote_rating")?,
                local_plays,
                remote_plays,
                combined_plays: local_plays + remote_plays,
            })
        })
        .collect()
}

async fn confirmed_link_on(
    conn: &mut SqliteConnection,
    local_track_id: i64,
) -> AppResult<(String, bool, bool, Option<i64>, Option<i64>)> {
    let row = sqlx::query(
        "SELECT l.remote_track_id, t.file_path, l.verified_full_hash,
                rt.full_hash AS remote_full_hash,
                EXISTS(SELECT 1 FROM liked_track f WHERE f.track_id = t.id) AS local_favorite,
                EXISTS(SELECT 1 FROM remote_favorite f
                        WHERE f.entity_type = 'track' AND f.entity_id = l.remote_track_id) AS remote_favorite,
                t.rating AS local_rating,
                (SELECT rating FROM remote_rating r
                  WHERE r.entity_type = 'track' AND r.entity_id = l.remote_track_id) AS remote_rating
           FROM remote_track_link l
           JOIN track t ON t.id = l.local_track_id AND t.is_available = 1
           JOIN remote_track rt ON rt.remote_id = l.remote_track_id
          WHERE l.local_track_id = ? AND l.status = 'confirmed'",
    )
    .bind(local_track_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::Other("confirmed visible reconciliation link not found".into()))?;
    let freshness = playlist_link_freshness(vec![PlaylistLinkProof {
        local_track_id,
        file_path: row.try_get("file_path")?,
        verified_full_hash: row.try_get("verified_full_hash")?,
        remote_full_hash: row.try_get("remote_full_hash")?,
    }])
    .await?;
    if freshness.get(&local_track_id) != Some(&true) {
        return Err(AppError::Other("reconciliation link is stale".into()));
    }
    Ok((
        row.try_get("remote_track_id")?,
        row.try_get::<i64, _>("local_favorite")? != 0,
        row.try_get::<i64, _>("remote_favorite")? != 0,
        row.try_get("local_rating")?,
        row.try_get("remote_rating")?,
    ))
}

pub async fn copy_favorite(
    pool: &SqlitePool,
    local_track_id: i64,
    direction: &str,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let (remote_id, local, remote, _, _) = confirmed_link_on(&mut tx, local_track_id).await?;
    match direction {
        "local_to_server" => {
            crate::remote::write::set_favorite_in_tx(&mut tx, "track", &remote_id, local).await?
        }
        "server_to_local" => {
            if remote {
                sqlx::query(
                    "INSERT OR REPLACE INTO liked_track (track_id, liked_at) VALUES (?, ?)",
                )
                .bind(local_track_id)
                .bind(now_ms())
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query("DELETE FROM liked_track WHERE track_id = ?")
                    .bind(local_track_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        _ => return Err(AppError::Other("invalid user-data copy direction".into())),
    }
    tx.commit().await?;
    Ok(())
}

pub async fn copy_rating(pool: &SqlitePool, local_track_id: i64, direction: &str) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let (remote_id, _, _, local, remote) = confirmed_link_on(&mut tx, local_track_id).await?;
    match direction {
        "local_to_server" => {
            let stars = local
                .map(|value| ((value.clamp(0, 255) * 5 + 127) / 255).clamp(1, 5) as u8)
                .unwrap_or(0);
            crate::remote::write::set_rating_in_tx(&mut tx, "track", &remote_id, stars).await?;
        }
        "server_to_local" => {
            // Reconciliation copies user data, never audio tags. A later local
            // edit may deliberately write the rating through the normal track
            // command, but this directional copy stays database-only.
            let popm = remote.map(|value| (value.clamp(1, 5) * 255 + 2) / 5);
            sqlx::query("UPDATE track SET rating = ? WHERE id = ?")
                .bind(popm)
                .bind(local_track_id)
                .execute(&mut *tx)
                .await?;
        }
        _ => return Err(AppError::Other("invalid user-data copy direction".into())),
    }
    tx.commit().await?;
    Ok(())
}

pub async fn set_playback_preference(
    pool: &SqlitePool,
    local_track_id: i64,
    preference: &str,
) -> AppResult<()> {
    if !matches!(preference, "local_first" | "server_first") {
        return Err(AppError::Other("invalid playback preference".into()));
    }
    let result = sqlx::query(
        "UPDATE remote_track_link SET playback_preference = ?
          WHERE local_track_id = ?",
    )
    .bind(preference)
    .bind(local_track_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::Other("reconciliation link not found".into()));
    }
    Ok(())
}

/// Resolve a remote track to the confirmed, available local file selected by
/// the user's playback preference. A missing file is treated like no local
/// candidate so callers can immediately fall back to the server.
/// The server track a local one was proved to be, for playing it when its
/// file is gone.
///
/// The mirror image of [`preferred_local_playback`], and the direction that
/// did not exist: server-to-local has always resolved, local-to-server never
/// did, so a library track whose file vanished simply failed. Only a
/// `confirmed` link qualifies — a stale one is a guess, and guessing which
/// recording to play instead is worse than saying the file is missing.
pub async fn linked_remote_track(
    pool: &SqlitePool,
    local_track_id: i64,
) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT remote_track_id FROM remote_track_link
          WHERE local_track_id = ? AND status = 'confirmed'
          LIMIT 1",
    )
    .bind(local_track_id)
    .fetch_optional(pool)
    .await?)
}

pub async fn preferred_local_playback(
    pool: &SqlitePool,
    remote_track_id: &str,
) -> AppResult<Option<PreferredLocalPlayback>> {
    let row = sqlx::query(
        "SELECT t.id, t.file_path, t.duration_ms
           FROM remote_track_link l
           JOIN track t ON t.id = l.local_track_id
          WHERE l.remote_track_id = ?
            AND l.status = 'confirmed'
            AND l.playback_preference = 'local_first'
            AND t.is_available = 1
          LIMIT 1",
    )
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let path = PathBuf::from(row.try_get::<String, _>("file_path")?);
    if !path.is_file() {
        return Ok(None);
    }
    let duration_ms = row.try_get::<i64, _>("duration_ms")?.max(0) as u64;
    Ok(Some(PreferredLocalPlayback {
        track_id: row.try_get("id")?,
        path,
        duration_ms,
    }))
}

/// Preview an explicit playlist conversion without mutating either source.
/// Only confirmed links are convertible; stale, missing and ambiguous pairs
/// remain visible in their original positions and block conversion.
pub async fn preview_playlist_conversion(
    pool: &SqlitePool,
    direction: &str,
    source_id: &str,
) -> AppResult<PlaylistConversionPreview> {
    let mut conn = pool.acquire().await?;
    preview_playlist_conversion_on(&mut conn, direction, source_id).await
}

async fn preview_playlist_conversion_on(
    conn: &mut SqliteConnection,
    direction: &str,
    source_id: &str,
) -> AppResult<PlaylistConversionPreview> {
    let (source_name, mut items) = match direction {
        "local_to_server" => {
            let playlist_id = source_id
                .parse::<i64>()
                .map_err(|_| AppError::Other("invalid local playlist id".into()))?;
            let row = sqlx::query("SELECT name, is_smart FROM playlist WHERE id = ?")
                .bind(playlist_id)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(|| AppError::Other("local playlist not found".into()))?;
            if row.try_get::<i64, _>("is_smart")? != 0 {
                return Err(AppError::Other(
                    "smart playlists must be materialized locally before conversion".into(),
                ));
            }
            let rows = sqlx::query(
                "SELECT pt.position, t.id AS local_track_id, t.title, t.file_path,
                        t.is_available, l.remote_track_id, l.status,
                        l.verified_full_hash, rt.full_hash AS remote_full_hash,
                        rt.remote_id IS NOT NULL AS remote_visible
                   FROM playlist_track pt
                   JOIN track t ON t.id = pt.track_id
                   LEFT JOIN remote_track_link l ON l.local_track_id = t.id
                   LEFT JOIN remote_track rt ON rt.remote_id = l.remote_track_id
                  WHERE pt.playlist_id = ?
                  ORDER BY pt.position, t.id",
            )
            .bind(playlist_id)
            .fetch_all(&mut *conn)
            .await?;
            let mut proofs = Vec::new();
            for row in &rows {
                if row.try_get::<i64, _>("is_available")? != 0 {
                    proofs.push(PlaylistLinkProof {
                        local_track_id: row.try_get("local_track_id")?,
                        file_path: row.try_get("file_path")?,
                        verified_full_hash: row.try_get("verified_full_hash")?,
                        remote_full_hash: row.try_get("remote_full_hash")?,
                    });
                }
            }
            let freshness = playlist_link_freshness(proofs).await?;
            let items = rows
                .into_iter()
                .map(|row| {
                    let local_track_id: i64 = row.try_get("local_track_id")?;
                    let remote_track_id: Option<String> = row.try_get("remote_track_id")?;
                    let link_status: Option<String> = row.try_get("status")?;
                    let local_available = row.try_get::<i64, _>("is_available")? != 0;
                    let remote_visible = row.try_get::<i64, _>("remote_visible")? != 0;
                    let confirmed_context = link_status.as_deref() == Some(STATUS_CONFIRMED)
                        && remote_track_id.is_some()
                        && local_available
                        && remote_visible;
                    let fresh = freshness.get(&local_track_id).copied().unwrap_or(false);
                    let status = if confirmed_context && fresh {
                        STATUS_CONFIRMED
                    } else if link_status.as_deref() == Some(STATUS_STALE)
                        || (confirmed_context && !fresh)
                    {
                        STATUS_STALE
                    } else {
                        "unlinked_or_ambiguous"
                    };
                    Ok(PlaylistConversionItem {
                        position: row.try_get("position")?,
                        title: row.try_get("title")?,
                        local_track_id: Some(local_track_id),
                        remote_track_id,
                        status: status.to_string(),
                    })
                })
                .collect::<AppResult<Vec<_>>>()?;
            (row.try_get("name")?, items)
        }
        "server_to_local" => {
            let row = sqlx::query("SELECT name FROM remote_playlist WHERE remote_id = ?")
                .bind(source_id)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(|| AppError::Other("server playlist not found".into()))?;
            let rows = sqlx::query(
                "SELECT rpt.position, rpt.track_remote_id, rt.title,
                        l.local_track_id, l.status, l.verified_full_hash,
                        rt.full_hash AS remote_full_hash, t.file_path,
                        rt.remote_id IS NOT NULL AS remote_visible,
                        t.id IS NOT NULL AND t.is_available = 1 AS local_visible
                   FROM remote_playlist_track rpt
                   LEFT JOIN remote_track rt ON rt.remote_id = rpt.track_remote_id
                   LEFT JOIN remote_track_link l ON l.remote_track_id = rpt.track_remote_id
                   LEFT JOIN track t ON t.id = l.local_track_id
                  WHERE rpt.playlist_remote_id = ?
                  ORDER BY rpt.position",
            )
            .bind(source_id)
            .fetch_all(&mut *conn)
            .await?;
            let mut proofs = Vec::new();
            for row in &rows {
                if row.try_get::<i64, _>("local_visible")? != 0 {
                    proofs.push(PlaylistLinkProof {
                        local_track_id: row.try_get("local_track_id")?,
                        file_path: row.try_get("file_path")?,
                        verified_full_hash: row.try_get("verified_full_hash")?,
                        remote_full_hash: row.try_get("remote_full_hash")?,
                    });
                }
            }
            let freshness = playlist_link_freshness(proofs).await?;
            let mut seen_local = HashSet::new();
            let items = rows
                .into_iter()
                .map(|row| {
                    let local_track_id: Option<i64> = row.try_get("local_track_id")?;
                    let link_status: Option<String> = row.try_get("status")?;
                    let remote_visible = row.try_get::<i64, _>("remote_visible")? != 0;
                    let local_visible = row.try_get::<i64, _>("local_visible")? != 0;
                    let fresh = local_track_id
                        .and_then(|id| freshness.get(&id).copied())
                        .unwrap_or(false);
                    let confirmed_context = link_status.as_deref() == Some(STATUS_CONFIRMED)
                        && remote_visible
                        && local_visible;
                    let status = if let Some(local_track_id) =
                        local_track_id.filter(|_| confirmed_context && fresh)
                    {
                        if seen_local.insert(local_track_id) {
                            STATUS_CONFIRMED
                        } else {
                            "duplicate"
                        }
                    } else if link_status.as_deref() == Some(STATUS_STALE)
                        || (confirmed_context && !fresh)
                    {
                        STATUS_STALE
                    } else {
                        "unlinked_or_ambiguous"
                    };
                    let remote_track_id: String = row.try_get("track_remote_id")?;
                    let title = row
                        .try_get::<Option<String>, _>("title")?
                        .unwrap_or_else(|| remote_track_id.clone());
                    Ok(PlaylistConversionItem {
                        position: row.try_get("position")?,
                        title,
                        local_track_id,
                        remote_track_id: Some(remote_track_id),
                        status: status.to_string(),
                    })
                })
                .collect::<AppResult<Vec<_>>>()?;
            (row.try_get("name")?, items)
        }
        _ => {
            return Err(AppError::Other(
                "invalid playlist conversion direction".into(),
            ))
        }
    };

    let total_tracks = items.len();
    let convertible_tracks = items
        .iter()
        .filter(|item| item.status == STATUS_CONFIRMED)
        .count();
    let blocked_tracks = total_tracks.saturating_sub(convertible_tracks);
    // Keep the original order in the response even if future status
    // enrichment appends rows from another source.
    items.sort_by_key(|item| item.position);
    Ok(PlaylistConversionPreview {
        direction: direction.to_string(),
        source_id: source_id.to_string(),
        source_name,
        total_tracks,
        convertible_tracks,
        blocked_tracks,
        can_convert: blocked_tracks == 0,
        items,
    })
}

/// Execute a previously previewable playlist conversion. The preview is
/// rebuilt inside the same transaction, preventing a link becoming stale or
/// disappearing between confirmation and mutation.
pub async fn convert_playlist(
    pool: &SqlitePool,
    direction: &str,
    source_id: &str,
) -> AppResult<PlaylistConversionResult> {
    convert_playlist_with_post_validation(pool, direction, source_id, || {}).await
}

async fn convert_playlist_with_post_validation<F>(
    pool: &SqlitePool,
    direction: &str,
    source_id: &str,
    post_validation: F,
) -> AppResult<PlaylistConversionResult>
where
    F: FnOnce(),
{
    let mut tx = pool.begin().await?;
    let preview = preview_playlist_conversion_on(&mut tx, direction, source_id).await?;
    if !preview.can_convert {
        return Err(AppError::Other(format!(
            "playlist conversion blocked by {} unlinked, stale, ambiguous, or duplicate tracks",
            preview.blocked_tracks
        )));
    }
    post_validation();

    let destination_id = match direction {
        "local_to_server" => {
            let track_ids = preview
                .items
                .iter()
                .filter_map(|item| item.remote_track_id.clone())
                .collect::<Vec<_>>();
            crate::remote::write::create_playlist_in_tx(&mut tx, &preview.source_name, &track_ids)
                .await?
        }
        "server_to_local" => {
            let now = now_ms();
            let draft = PlaylistDraft {
                name: preview.source_name.clone(),
                description: Some("Materialized from WaveFlow Server".into()),
                color_id: "violet".into(),
                color_mode: "auto".into(),
                icon_id: "music".into(),
                now_ms: now,
            };
            let playlist_id = insert_custom_conn(&mut tx, &draft).await?;
            let track_ids = preview
                .items
                .iter()
                .filter_map(|item| item.local_track_id)
                .collect::<Vec<_>>();
            append_tracks_conn(&mut tx, playlist_id, &track_ids, now).await?;
            playlist_id.to_string()
        }
        _ => unreachable!("direction validated by preview"),
    };
    let final_preview = preview_playlist_conversion_on(&mut tx, direction, source_id).await?;
    if !final_preview.can_convert {
        return Err(AppError::Other(format!(
            "playlist conversion blocked by {} tracks that changed during conversion",
            final_preview.blocked_tracks
        )));
    }
    tx.commit().await?;
    Ok(PlaylistConversionResult {
        direction: direction.to_string(),
        destination_id,
        converted_tracks: preview.total_tracks,
    })
}

pub async fn remove_link(pool: &SqlitePool, local_track_id: i64) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    // Capture the link's matching evidence before deleting it and record a
    // rejection for the same pair — otherwise the next `discover` would just
    // auto-recreate the exact link the user manually removed. Same proof
    // representation as `reject_exact`, so `reconcile` honours it.
    if let Some(row) = sqlx::query(
        "SELECT remote_track_id, method, verified_full_hash
           FROM remote_track_link WHERE local_track_id = ?",
    )
    .bind(local_track_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        let remote_track_id: String = row.try_get("remote_track_id")?;
        let method: String = row.try_get("method")?;
        let verified_full_hash: Option<String> = row.try_get("verified_full_hash")?;
        // Only an exact-hash link carries a full-hash proof to suppress.
        if method == "exact_full_hash" {
            if let Some(proof) = verified_full_hash.filter(|value| valid_full_hash(value)) {
                sqlx::query(
                    "INSERT OR IGNORE INTO remote_track_match_rejection
                        (local_track_id, remote_track_id, proof_kind, proof, rejected_at)
                     VALUES (?, ?, 'exact_full_hash', ?, ?)",
                )
                .bind(local_track_id)
                .bind(&remote_track_id)
                .bind(proof.to_ascii_lowercase())
                .bind(now_ms())
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    sqlx::query("DELETE FROM remote_track_link WHERE local_track_id = ?")
        .bind(local_track_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
