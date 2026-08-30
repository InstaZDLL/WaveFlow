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
    let remote_tracks = load_remote_tracks(pool).await?;
    if remote_tracks.is_empty() {
        // Nothing to reconcile, but this is still a terminal point: arbitrate
        // the phase exactly like `reconcile` so a cancel that raced this path is
        // reported consistently instead of as a plain empty result.
        return Ok(take_commit_slot(cancellable).unwrap_or_else(|| empty_report(false)));
    }

    let local_tracks = load_local_candidates(pool).await?;
    let total = local_tracks.len();
    let scan = tokio::task::spawn_blocking(move || {
        hash_local_tracks(local_tracks, app.as_ref(), total, cancellable)
    })
    .await
    .map_err(|err| AppError::Other(format!("reconciliation hash task failed: {err}")))?;

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
        "SELECT t.id, t.title, t.file_path, t.file_size, al.title AS album,
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
            })
        })
        .collect()
}

fn hash_local_tracks(
    tracks: Vec<LocalTrack>,
    app: Option<&AppHandle>,
    total: usize,
    cancellable: bool,
) -> HashScan {
    // The phase is only meaningful on the cancellable path; the no-progress/test
    // path must not read the shared phase (see `discover_inner`).
    let mut hashed = Vec::with_capacity(tracks.len());
    let mut unreadable = 0;
    for track in tracks {
        // Poll for a cancel at the top of the loop so a click that lands
        // between two files is honoured before starting another full-file read.
        if cancellable && RECONCILE_PHASE.load(Ordering::Relaxed) == PHASE_CANCELLED {
            return cancelled_scan(hashed, unreadable, total);
        }
        match waveflow_core::scanner::hash_file_full(Path::new(&track.file_path)) {
            Ok(full_hash) => hashed.push(HashedLocalTrack { track, full_hash }),
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
    }
    // A cancel arriving while the LAST file hashes would never be seen by the
    // top-of-loop poll, so the run would persist despite the click. Re-check
    // once after the loop before declaring the scan complete. The definitive
    // arbitration still happens at the pre-commit compare-exchange in
    // `reconcile`, which catches a cancel this relaxed load may have missed.
    if cancellable && RECONCILE_PHASE.load(Ordering::Relaxed) == PHASE_CANCELLED {
        return cancelled_scan(hashed, unreadable, total);
    }
    HashScan {
        hashed,
        unreadable,
        cancelled: false,
    }
}

fn cancelled_scan(hashed: Vec<HashedLocalTrack>, unreadable: usize, total: usize) -> HashScan {
    let processed = hashed.len() + unreadable;
    tracing::info!(processed, total, "reconciliation scan cancelled by user");
    HashScan {
        hashed,
        unreadable,
        cancelled: true,
    }
}

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

/// Confirms an exact local and remote track pairing after revalidating the current local file contents.
///
/// # Errors
///
/// Returns an error if either track is unavailable, their sizes or remote digest are invalid,
/// the local file cannot be hashed, or the current contents do not match the remote digest.
///
/// # Examples
///
/// ```no_run
/// # async fn example(pool: &sqlx::SqlitePool) -> waveflow_core::AppResult<()> {
/// waveflow_core::reconciliation::confirm_exact(pool, 42, "remote-track-id").await?;
/// # Ok(())
/// # }
/// ```
pub async fn confirm_exact(
    pool: &SqlitePool,
    local_track_id: i64,
    remote_track_id: &str,
) -> AppResult<()> {
    let row = sqlx::query(
        "SELECT t.file_path, t.file_size, rt.size AS remote_size, rt.full_hash
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
    let remote_size: i64 = row.try_get("remote_size")?;
    let remote_hash: String = row.try_get("full_hash")?;
    if local_size != remote_size || !valid_full_hash(&remote_hash) {
        return Err(AppError::Other(
            "tracks do not share a valid exact-content candidate".into(),
        ));
    }

    let local_hash = tokio::task::spawn_blocking(move || {
        waveflow_core::scanner::hash_file_full(Path::new(&file_path))
    })
    .await
    .map_err(|err| AppError::Other(format!("reconciliation hash task failed: {err}")))??;
    if !local_hash.eq_ignore_ascii_case(&remote_hash) {
        return Err(AppError::Other("track contents no longer match".into()));
    }

    link_exact(pool, local_track_id, remote_track_id, &remote_hash).await
}

/// Creates or updates a confirmed exact-hash link between a local and remote track.
///
/// The digest is normalized to lowercase, existing links on either side are
/// protected, and any matching exact-hash rejection is removed.
///
/// # Examples
///
/// ```ignore
/// link_exact(&pool, local_track_id, remote_track_id, full_hash).await?;
/// ```
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
mod tests {
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
        let scan = hash_local_tracks(locals, None, 0, false);
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
        sqlx::query(
            "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'Test', 0, 0)",
        )
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
        let changed_without_discover =
            preview_playlist_conversion(&pool, "server_to_local", "valid")
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

        let raced =
            convert_playlist_with_post_validation(&pool, "server_to_local", "valid", || {
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
}
