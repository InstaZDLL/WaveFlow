//! Release Radar: the scan that asks MusicBrainz about the library's
//! artists, and the reads the view and the playlist are built from.
//!
//! The pure half — the query, the parsing, the matching, the tables —
//! is [`waveflow_core::release_radar`]. What lives here is what needs
//! the app: the offline switch, the task row, the pacing across awaits,
//! and the profile the scan was started for.
//!
//! # The scan
//!
//! One request per followed artist, at most one a second, as MusicBrainz
//! asks. An artist answered less than [`REFRESH_AFTER_MS`] ago is not
//! asked again, so a scan that was stopped — by the user, by a rate
//! limit, by offline mode, by a profile switch — resumes with the
//! artists it did not reach: `release_radar_artist.checked_at` is the
//! cursor. A `503` / `429` is waited out once, at the delay MusicBrainz
//! gives, and stops the scan the second time.
//!
//! The pool is taken again for every artist rather than held for the
//! whole minute: a lease held across the pauses would make a profile
//! switch wait out its drain timeout, and the scan stops as soon as the
//! active profile is not the one it started for.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use waveflow_core::release_radar::{
    self,
    musicbrainz::{MusicBrainzClient, MusicBrainzError},
    store, ArtistQuery, MatchedBy, ReleaseRef, COVER_RETRY_AFTER_MS, FOLLOWED_ARTISTS, RECENT_DAYS,
    REFRESH_AFTER_MS,
};
use waveflow_core::smart_playlists::release_radar::regenerate_release_radar;

use crate::error::{AppError, AppResult};
use crate::smart_playlists::PathsContext;
use crate::state::AppState;

/// Emitted after every artist a scan records and when it ends, so an
/// open view redraws as releases come in.
pub const RELEASE_RADAR_CHANGED: &str = "release-radar:changed";

/// Gap between two MusicBrainz requests: their guidance is one a
/// second on average, and a little over keeps the average under.
const MB_SPACING: Duration = Duration::from_millis(1100);

/// Gap between two Cover Art Archive requests. It sets no rate of its
/// own, and a burst of a dozen covers is still no reason to hurry.
const COVER_SPACING: Duration = Duration::from_millis(250);

/// The wait after a rate limit that came without a `Retry-After`.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(5);

/// The shortest wait after a rate limit, whatever `Retry-After` says:
/// MusicBrainz answers its `503` with `Retry-After: 0`, and asking again
/// a second later is asking again at the rate it just refused.
const MIN_RETRY_AFTER: Duration = Duration::from_secs(3);

/// Transport failures in a row after which the scan stops: the network
/// is down, and asking the next sixty artists would only log sixty
/// failures.
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// How often a scan parked behind a library scan looks again.
const SCAN_WAIT_POLL: Duration = Duration::from_millis(500);

/// Attempts at a write that keeps meeting a busy database.
const MAX_WRITE_ATTEMPTS: u32 = 5;

/// One scan at a time: two would share MusicBrainz's one request a
/// second between them and record the same artists twice.
static SCAN_RUNNING: AtomicBool = AtomicBool::new(false);

/// Clears [`SCAN_RUNNING`] on every exit path.
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        SCAN_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// How a scan ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanOutcome {
    /// Every due artist was asked.
    Done,
    Cancelled,
    /// MusicBrainz asked twice in a row to slow down.
    RateLimited,
    /// Offline mode was on, or was turned on during the scan.
    Offline,
    /// The network failed several times in a row.
    Failed,
    /// The active profile changed; the scan stopped writing to the old one.
    ProfileChanged,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseRadarScanSummary {
    pub outcome: ScanOutcome,
    /// Artists asked and recorded by this scan.
    pub checked: u32,
    /// Artists the scan set out to ask.
    pub due: u32,
}

/// What [`RELEASE_RADAR_CHANGED`] carries.
#[derive(Debug, Clone, Serialize)]
struct RadarProgress {
    checked: u32,
    due: u32,
    running: bool,
}

/// Ask MusicBrainz about the followed artists that are due, cache the
/// new releases' covers, then rewrite the Release Radar playlist.
///
/// `rescan` asks about every followed artist again, however recently.
/// `playlist_description` is the playlist's description in the user's
/// language, which only the frontend knows.
#[tauri::command]
pub async fn release_radar_scan(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    rescan: bool,
    playlist_description: String,
) -> AppResult<ReleaseRadarScanSummary> {
    if crate::offline::is_offline() {
        return Ok(ReleaseRadarScanSummary {
            outcome: ScanOutcome::Offline,
            checked: 0,
            due: 0,
        });
    }
    if SCAN_RUNNING.swap(true, Ordering::SeqCst) {
        return Err(AppError::Other("release radar scan already running".into()));
    }
    let running = RunningGuard;
    let summary = run_scan(&app, &state, rescan, &playlist_description).await;
    // Cleared before the last event: a view that reloads on it must read
    // the scan as over.
    drop(running);
    let _ = app.emit(
        RELEASE_RADAR_CHANGED,
        RadarProgress {
            checked: summary.as_ref().map_or(0, |s| s.checked),
            due: summary.as_ref().map_or(0, |s| s.due),
            running: false,
        },
    );
    summary
}

async fn run_scan(
    app: &AppHandle,
    state: &tauri::State<'_, AppState>,
    rescan: bool,
    playlist_description: &str,
) -> AppResult<ReleaseRadarScanSummary> {
    let now = Utc::now();
    let cutoff = release_radar::cutoff_date(now, RECENT_DAYS);
    // Announced before anything waits, with the total unknown until the
    // followed artists are read: a scan parked behind a library scan is
    // still a scan the user can see and stop.
    let task = crate::tasks::start(
        app,
        crate::tasks::TaskKind::ReleaseRadar,
        0,
        crate::tasks::Cancellation::Flag,
    );
    let cancelled = || task.as_ref().is_some_and(|t| t.is_cancelling());

    wait_out_library_scan(&cancelled).await;
    if cancelled() {
        return Ok(ReleaseRadarScanSummary {
            outcome: ScanOutcome::Cancelled,
            checked: 0,
            due: 0,
        });
    }
    let (due, profile_id) = {
        let (pool, profile_id) = state.require_profile_snapshot().await?;
        if rescan {
            with_busy_retry(|| store::reset_checks(&pool)).await?;
        }
        with_busy_retry(|| store::prune_older_than(&pool, &cutoff)).await?;
        let now_ms = now.timestamp_millis();
        let due: Vec<_> = store::followed_artists(&pool, now_ms, FOLLOWED_ARTISTS)
            .await?
            .into_iter()
            .filter(|a| is_due(a.checked_at, now_ms))
            .collect();
        (due, profile_id)
    };

    let total = due.len() as u32;
    if let Some(task) = task.as_ref() {
        task.progress(0, u64::from(total));
    }

    let client = MusicBrainzClient::new();
    let mut last_request: Option<Instant> = None;
    let mut checked = 0u32;
    let mut failures = 0u32;
    let mut outcome = ScanOutcome::Done;

    'artists: for artist in &due {
        // Park while a library scan holds the writer: the radar is
        // background work and can wait a scan out.
        wait_out_library_scan(&cancelled).await;
        if cancelled() {
            outcome = ScanOutcome::Cancelled;
            break;
        }
        if let Some(task) = task.as_ref() {
            task.detail(artist.name.clone());
        }
        let query = match artist.mbid.as_deref() {
            Some(mbid) => ArtistQuery::Mbid(mbid),
            None => ArtistQuery::Name(&artist.name),
        };

        let mut waited_out = false;
        let page = loop {
            // Checked before every request, not once per scan: the
            // switch can be turned on while the scan runs.
            if crate::offline::is_offline() {
                outcome = ScanOutcome::Offline;
                break 'artists;
            }
            if let Some(at) = last_request {
                let elapsed = at.elapsed();
                if elapsed < MB_SPACING {
                    tokio::time::sleep(MB_SPACING - elapsed).await;
                }
            }
            last_request = Some(Instant::now());
            match client.recent_release_groups(&query, &cutoff).await {
                Ok(page) => break Some(page),
                Err(MusicBrainzError::RateLimited { retry_after }) if !waited_out => {
                    waited_out = true;
                    let wait = retry_after
                        .unwrap_or(DEFAULT_RETRY_AFTER)
                        .max(MIN_RETRY_AFTER);
                    tracing::info!(?wait, "release radar: MusicBrainz asked to slow down");
                    if !sleep_unless_cancelled(wait, &cancelled).await {
                        outcome = ScanOutcome::Cancelled;
                        break 'artists;
                    }
                }
                Err(MusicBrainzError::RateLimited { .. }) => {
                    outcome = ScanOutcome::RateLimited;
                    break 'artists;
                }
                Err(err) => {
                    // The artist stays due: the next scan asks again.
                    tracing::warn!(artist_id = artist.artist_id, %err, "release radar: lookup failed");
                    // Only a transport failure says the network is down; a
                    // status or a body MusicBrainz sent says it is up.
                    if matches!(err, MusicBrainzError::Http(_)) {
                        failures += 1;
                    } else {
                        failures = 0;
                    }
                    break None;
                }
            }
        };
        let Some(page) = page else {
            if failures >= MAX_CONSECUTIVE_FAILURES {
                outcome = ScanOutcome::Failed;
                break;
            }
            continue;
        };
        failures = 0;

        let (pool, active) = state.require_profile_snapshot().await?;
        if active != profile_id {
            outcome = ScanOutcome::ProfileChanged;
            break;
        }
        // An id is learnt only off a release the library holds: the
        // answer alone cannot tell the artist from a homonym.
        let mbid = if artist.mbid.is_none() && !page.releases.is_empty() {
            let tracks = store::local_tracks_of(&pool, artist.artist_id).await?;
            let refs: Vec<ReleaseRef> = page
                .releases
                .iter()
                .map(|r| ReleaseRef {
                    id: r.id.clone(),
                    artist_id: artist.artist_id,
                    title: r.title.clone(),
                    primary_type: r.primary_type.clone(),
                })
                .collect();
            release_radar::confirmed_mbid(
                &page.releases,
                &release_radar::match_releases(&refs, &tracks),
            )
        } else {
            None
        };
        let recorded_at = Utc::now().timestamp_millis();
        with_busy_retry(|| {
            store::record_artist(
                &pool,
                artist.artist_id,
                mbid.as_deref(),
                &page.releases,
                recorded_at,
            )
        })
        .await?;
        drop(pool);
        checked += 1;
        if let Some(task) = task.as_ref() {
            task.progress(u64::from(checked), u64::from(total));
        }

        if !page.releases.is_empty() {
            match fetch_covers(state, profile_id, &cancelled).await? {
                CoversEnd::Done => {}
                CoversEnd::Offline => {
                    outcome = ScanOutcome::Offline;
                    break;
                }
                CoversEnd::ProfileChanged => {
                    outcome = ScanOutcome::ProfileChanged;
                    break;
                }
            }
        }
        let _ = app.emit(
            RELEASE_RADAR_CHANGED,
            RadarProgress {
                checked,
                due: total,
                running: true,
            },
        );
    }

    // The playlist follows what was found, however the scan ended —
    // unless the profile is no longer the one it was found for.
    if outcome != ScanOutcome::ProfileChanged {
        // Not cancellable: what was found is written, stop or not.
        wait_out_library_scan(&|| false).await;
        let (pool, active) = state.require_profile_snapshot().await?;
        if active == profile_id {
            let paths = paths_ctx(state, profile_id);
            let id = with_busy_retry(|| {
                regenerate_release_radar(&pool, &paths, profile_id, playlist_description)
            })
            .await?;
            let _ = app.emit("playlist:changed", id);
        }
    }

    Ok(ReleaseRadarScanSummary {
        outcome,
        checked,
        due: total,
    })
}

enum CoversEnd {
    Done,
    Offline,
    ProfileChanged,
}

/// Cache the Cover Art Archive front of every release still without
/// one, into the shared `metadata_artwork/` cache, so the view draws
/// local files — offline too — and never a remote image.
async fn fetch_covers(
    state: &tauri::State<'_, AppState>,
    profile_id: i64,
    cancelled: &impl Fn() -> bool,
) -> AppResult<CoversEnd> {
    let due = {
        let (pool, active) = state.require_profile_snapshot().await?;
        if active != profile_id {
            return Ok(CoversEnd::ProfileChanged);
        }
        let now_ms = Utc::now().timestamp_millis();
        store::covers_due(&pool, now_ms, COVER_RETRY_AFTER_MS).await?
    };
    let dir = state.paths.metadata_artwork_dir.clone();
    for (i, release_id) in due.iter().enumerate() {
        if cancelled() {
            break;
        }
        if crate::offline::is_offline() {
            return Ok(CoversEnd::Offline);
        }
        if i > 0 {
            tokio::time::sleep(COVER_SPACING).await;
        }
        // `None` both for "the archive has no cover" (a 404) and for a
        // failed download: either way it is asked again in a week.
        let hash = crate::metadata_artwork::download_and_cache(
            &release_radar::cover_url(release_id),
            &dir,
        )
        .await;
        let (pool, active) = state.require_profile_snapshot().await?;
        if active != profile_id {
            return Ok(CoversEnd::ProfileChanged);
        }
        let now_ms = Utc::now().timestamp_millis();
        with_busy_retry(|| store::record_cover(&pool, release_id, hash.as_deref(), now_ms)).await?;
    }
    Ok(CoversEnd::Done)
}

/// Wait while a library scan holds the single writer, or until `cancelled`.
async fn wait_out_library_scan(cancelled: &impl Fn() -> bool) {
    while crate::commands::scan::scan_in_flight() && !cancelled() {
        tokio::time::sleep(SCAN_WAIT_POLL).await;
    }
}

/// Sleep for `wait`, waking every [`SCAN_WAIT_POLL`] to see whether the
/// scan was stopped: a `Retry-After` can be half a minute, and a stop
/// button that takes that long to answer reads as broken. `false` when
/// the scan was stopped.
async fn sleep_unless_cancelled(wait: Duration, cancelled: &impl Fn() -> bool) -> bool {
    let until = Instant::now() + wait;
    loop {
        if cancelled() {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        tokio::time::sleep(left.min(SCAN_WAIT_POLL)).await;
    }
}

/// Run a write again while it meets a busy database, with a short
/// backoff — a library scan or a play being recorded can hold the
/// single writer for a moment, and a lost write here would make the
/// scan ask MusicBrainz about the same artist again.
async fn with_busy_retry<T, F, Fut>(mut write: F) -> AppResult<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = waveflow_core::error::CoreResult<T>>,
{
    let mut attempt = 1;
    loop {
        match write().await {
            Ok(value) => return Ok(value),
            Err(waveflow_core::error::CoreError::Database(err))
                if attempt < MAX_WRITE_ATTEMPTS && crate::commands::analysis::is_busy(&err) =>
            {
                tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
                attempt += 1;
            }
            Err(err) => return Err(err.into()),
        }
    }
}

/// Whether an artist last answered at `checked_at` is asked again: never
/// asked, or asked more than [`REFRESH_AFTER_MS`] ago.
fn is_due(checked_at: Option<i64>, now_ms: i64) -> bool {
    !matches!(checked_at, Some(at) if at >= now_ms - REFRESH_AFTER_MS)
}

fn paths_ctx(state: &AppState, profile_id: i64) -> PathsContext {
    PathsContext {
        metadata_artwork_dir: state.paths.metadata_artwork_dir.clone(),
        app_db_path: state.paths.app_db.clone(),
        profile_artwork_dir: state.paths.profile_artwork_dir(profile_id),
    }
}

/// One release as the view shows it.
#[derive(Debug, Clone, Serialize)]
pub struct RadarRelease {
    pub id: String,
    pub title: String,
    pub artist_id: i64,
    pub artist_name: String,
    pub artist_credit: String,
    pub first_release_date: String,
    pub primary_type: Option<String>,
    pub secondary_types: Vec<String>,
    /// The cached cover, when the archive had one and the file is there.
    pub cover_path: Option<String>,
    pub musicbrainz_url: String,
    /// The local tracks matched to it, in play order. Empty means the
    /// release is only discovered — never offered as playable.
    pub track_ids: Vec<i64>,
    /// How the tracks were found; `None` with no track.
    pub matched_by: Option<MatchedBy>,
    pub discovered_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseRadarSnapshot {
    /// Newest first.
    pub releases: Vec<RadarRelease>,
    /// Artists the radar follows.
    pub followed: u32,
    /// Of those, the ones MusicBrainz has answered for.
    pub checked: u32,
    /// Of those, the ones due again: what the next scan asks about.
    pub due: u32,
    /// The latest answer, epoch ms; `None` before the first scan.
    pub last_checked_at: Option<i64>,
    pub scanning: bool,
    /// The Release Radar playlist, when there is one.
    pub playlist_id: Option<i64>,
}

/// Everything the Release Radar view draws. Reads only — the database
/// and the cover cache, never the network — so it answers offline too.
#[tauri::command]
pub async fn release_radar_list(
    state: tauri::State<'_, AppState>,
) -> AppResult<ReleaseRadarSnapshot> {
    let (pool, _profile_id) = state.require_profile_snapshot().await?;
    let now = Utc::now();
    let now_ms = now.timestamp_millis();
    let cutoff = release_radar::cutoff_date(now, RECENT_DAYS);

    let stored = store::recent_releases(&pool, &cutoff).await?;
    let refs: Vec<_> = stored.iter().map(|r| r.as_ref()).collect();
    let tracks = store::local_tracks(&pool).await?;
    let mut matches = release_radar::match_releases(&refs, &tracks);
    let dir = &state.paths.metadata_artwork_dir;
    let releases = stored
        .into_iter()
        .map(|r| {
            let found = matches.remove(&r.id);
            RadarRelease {
                cover_path: r
                    .cover_hash
                    .as_deref()
                    .and_then(|h| crate::metadata_artwork::existing_path(dir, h)),
                musicbrainz_url: release_radar::musicbrainz_url(&r.id),
                matched_by: found.as_ref().map(|m| m.matched_by),
                track_ids: found.map(|m| m.track_ids).unwrap_or_default(),
                secondary_types: r
                    .secondary_types
                    .as_deref()
                    .map(|s| s.split("; ").map(str::to_string).collect())
                    .unwrap_or_default(),
                id: r.id,
                title: r.title,
                artist_id: r.artist_id,
                artist_name: r.artist_name,
                artist_credit: r.artist_credit,
                first_release_date: r.first_release_date,
                primary_type: r.primary_type,
                discovered_at: r.discovered_at,
            }
        })
        .collect();

    let followed = store::followed_artists(&pool, now_ms, FOLLOWED_ARTISTS).await?;
    let checked = followed.iter().filter(|a| a.checked_at.is_some()).count() as u32;
    let due = followed
        .iter()
        .filter(|a| is_due(a.checked_at, now_ms))
        .count() as u32;
    let last_checked_at = followed.iter().filter_map(|a| a.checked_at).max();
    let playlist_id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM playlist WHERE is_smart = 1 AND smart_rules LIKE ? LIMIT 1",
    )
    .bind("%\"kind\":\"release_radar\"%")
    .fetch_optional(&*pool)
    .await?;

    Ok(ReleaseRadarSnapshot {
        releases,
        followed: followed.len() as u32,
        checked,
        due,
        last_checked_at,
        scanning: SCAN_RUNNING.load(Ordering::SeqCst),
        playlist_id,
    })
}

/// Rewrite the Release Radar playlist from what is already known — the
/// stored releases and the library as it is now. Local only, so the
/// view calls it on opening: an album imported since the last scan
/// joins the playlist without asking MusicBrainz anything.
#[tauri::command]
pub async fn release_radar_refresh_playlist(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_description: String,
) -> AppResult<Option<i64>> {
    wait_out_library_scan(&|| false).await;
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let paths = paths_ctx(&state, profile_id);
    let id = with_busy_retry(|| {
        regenerate_release_radar(&pool, &paths, profile_id, &playlist_description)
    })
    .await?;
    let _ = app.emit("playlist:changed", id);
    Ok(id)
}

#[cfg(test)]
mod tests;
