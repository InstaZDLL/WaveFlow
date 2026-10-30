//! One enrichment pass: walk what is pending, ask MusicBrainz, write the
//! decisions.
//!
//! The matching and the gap filling are `waveflow_core::enrichment`'s;
//! this is the part that needs the app — the profile pool, the task
//! registry, the artwork folder, the scan to stay out of the way of.
//!
//! # A second background writer
//!
//! Same shape as the analysis sweep (`docs/architecture/invariants.md`,
//! "Single writer to SQLite"): every write parks while a scan is in
//! flight, goes into one short transaction, and is retried on
//! `SQLITE_BUSY` / `SQLITE_LOCKED`. The network wait is never inside a
//! transaction — an album is resolved first, written after.
//!
//! # Stopping
//!
//! Between two albums or two artists, through the task registry's flag.
//! Every album is written whole or not at all, so a stop leaves nothing
//! half-applied, and what was not reached stays pending for the next
//! pass.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Manager};

use waveflow_core::enrichment::matching::Verdict;
use waveflow_core::enrichment::resolve::{self, Resolution};
use waveflow_core::enrichment::store::{self, ApplyReport, Fields, Status};
use waveflow_core::enrichment::{tags, ArtistProposal, LocalAlbum};
use waveflow_core::error::CoreError;
use waveflow_core::metadata::coverartarchive::{CoverArtArchiveClient, CoverOf};
use waveflow_core::metadata::musicbrainz::{shared_pacer, MusicBrainzClient};
use waveflow_core::metadata::name_match::names_match;
use waveflow_core::scanner::{upsert_artwork, ArtworkSource, VARIOUS_ARTISTS_LABEL};

use crate::error::AppResult;
use crate::state::AppState;

use super::{read_enabled, read_fields, CHANGED_EVENT};

/// A `no_match` is asked again after this long: the catalogue grows.
pub(super) const NO_MATCH_RETRY_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Albums read from the database at a time.
const BATCH: i64 = 20;
/// Consecutive failed lookups after which the pass gives up: the
/// service is down or unreachable, and asking a thousand more times
/// changes nothing but the log.
const MAX_CONSECUTIVE_FAILURES: u32 = 5;
/// Smallest gap between two `library:rescanned` emissions during a
/// pass. Every listener reloads what it shows; once a minute keeps the
/// filled genres and covers appearing without reloading the library
/// view every few seconds.
const LIBRARY_EMIT_GAP: Duration = Duration::from_secs(60);
/// MusicBrainz's own identifier for its "Various Artists" entry: a
/// compilation credit, never an artist to propose.
const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";
const SCAN_WAIT_POLL: Duration = Duration::from_millis(250);

/// The one pass slot, and whether a run was asked for while it was
/// taken — the analysis sweep's `Slot`, for the same reasons.
struct Slot {
    running: bool,
    rerun: bool,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot {
    running: false,
    rerun: false,
});
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Set when the profile the pass reads from is being closed. It stops the
/// pass like [`CANCEL`] does, but it is not the user's stop: the guard
/// still starts the run asked for, which reads the profile opened next.
static PROFILE_CHANGE: AtomicBool = AtomicBool::new(false);

fn slot() -> std::sync::MutexGuard<'static, Slot> {
    SLOT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn reserve(queue: bool) -> bool {
    let mut slot = slot();
    if slot.running {
        if queue {
            slot.rerun = true;
        }
        return false;
    }
    slot.running = true;
    slot.rerun = false;
    CANCEL.store(false, Ordering::SeqCst);
    PROFILE_CHANGE.store(false, Ordering::SeqCst);
    true
}

/// Ask the running pass to stop. `false` when nothing ran.
pub(super) fn request_stop() -> bool {
    let slot = slot();
    if slot.running {
        CANCEL.store(true, Ordering::SeqCst);
    }
    slot.running
}

pub(super) fn is_running() -> bool {
    slot().running
}

/// The active profile is about to be closed: stop the running pass at its
/// next stopping point, and run again afterwards for whichever profile is
/// active then.
///
/// Called by [`crate::state::AppState::activate_profile`] and
/// `deactivate_profile` once the old profile is out of the state, before
/// they wait for its leases. A pass holds its lease for hours; without
/// this the switch waits out the full drain timeout and then closes the
/// pool under the pass, which goes on sending the old profile's titles to
/// MusicBrainz until its next write fails.
pub fn stop_for_profile_change() {
    let mut slot = slot();
    if slot.running {
        PROFILE_CHANGE.store(true, Ordering::SeqCst);
        slot.rerun = true;
    }
}

/// Releases the slot on every exit path, and starts the run that was
/// asked for meanwhile — unless the user stopped this one.
struct RunningGuard {
    app: AppHandle,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let rerun = {
            let mut slot = slot();
            let wanted = slot.rerun && !CANCEL.load(Ordering::SeqCst);
            slot.running = false;
            slot.rerun = false;
            wanted
        };
        let _ = self.app.emit(CHANGED_EVENT, ());
        if rerun {
            maybe_run(&self.app);
        }
    }
}

/// Start a pass in the background if the active profile has the engine
/// on. Called after a scan, when a profile opens, and when the user
/// turns the engine on or asks for a pass. A pass already running
/// records the request and honours it once it ends.
pub fn maybe_run(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let Ok((pool, profile_id)) = state.require_profile_snapshot().await else {
            return;
        };
        let enabled = read_enabled(&pool).await;
        // Not held into the pass: see `ProfileDb`.
        drop(pool);
        if !enabled || crate::offline::is_offline() {
            return;
        }
        if !reserve(true) {
            return;
        }
        let _guard = RunningGuard { app: app.clone() };
        let artwork_dir = state.paths.profile_artwork_dir(profile_id);
        let db = ProfileDb::new(&app, profile_id);
        if let Err(err) = run_pass(&app, &db, &artwork_dir).await {
            tracing::warn!(%err, "enrichment pass failed");
        }
    });
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn cancelled() -> bool {
    CANCEL.load(Ordering::Relaxed) || PROFILE_CHANGE.load(Ordering::Relaxed)
}

/// Park while a scan walks and writes. Cancel-aware.
async fn wait_out_active_scan() {
    while crate::commands::scan::scan_in_flight() {
        if cancelled() {
            return;
        }
        tokio::time::sleep(SCAN_WAIT_POLL).await;
    }
}

/// A lookup failure that belongs to this album and asking again will not
/// change: its release is too large to read, or its query is one the
/// service refuses as malformed.
///
/// Deliberately narrow. A `403`, a `401`, or an answer that no longer
/// parses is the same for every album — a block, a change of format —
/// and reading it as "this album has no match" would mark the whole
/// library unmatched for a month. Those stay failures, which end the pass.
fn is_permanent(err: &waveflow_core::metadata::LookupError) -> bool {
    use waveflow_core::metadata::LookupError as E;
    match err {
        E::TooLarge { .. } => true,
        E::Status { status } => matches!(status, 400 | 413 | 414 | 422),
        E::Decode(_) | E::Refused { .. } | E::Http(_) => false,
    }
}

fn is_busy(err: &CoreError) -> bool {
    matches!(err, CoreError::Database(e) if crate::commands::analysis::is_busy(e))
}

/// Run `write` in a transaction, retried on a busy database; `write` is
/// called afresh on every attempt.
///
/// `park` waits out a running scan before each attempt — what a
/// background writer owes the scan. A write the user just asked for does
/// not: it is one short transaction, the busy retry covers it, and
/// making a button wait for a library scan to end is a button that looks
/// broken.
async fn write_with<T, F>(pool: &SqlitePool, park: bool, mut write: F) -> Result<T, CoreError>
where
    F: for<'c> FnMut(
        &'c mut sqlx::SqliteConnection,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, CoreError>> + Send + 'c>,
    >,
{
    const MAX_ATTEMPTS: usize = 6;
    let mut backoff = Duration::from_millis(50);
    let mut attempt = 0;
    loop {
        attempt += 1;
        if park {
            wait_out_active_scan().await;
        }
        let result = async {
            let mut tx = pool.begin().await?;
            let value = write(&mut tx).await?;
            tx.commit().await?;
            Ok::<T, CoreError>(value)
        }
        .await;
        match result {
            Err(err) if is_busy(&err) && attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(2));
            }
            other => return other,
        }
    }
}

/// A write a command makes for the user: see [`write_with`].
pub(super) async fn write_now<T, F>(pool: &SqlitePool, write: F) -> Result<T, CoreError>
where
    F: for<'c> FnMut(
        &'c mut sqlx::SqliteConnection,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, CoreError>> + Send + 'c>,
    >,
{
    write_with(pool, false, write).await
}

/// The clients one pass uses, sharing the process-wide pacing.
pub(super) fn clients() -> (MusicBrainzClient, CoverArtArchiveClient) {
    let version = env!("CARGO_PKG_VERSION");
    (
        MusicBrainzClient::new(version),
        CoverArtArchiveClient::new(version, shared_pacer()),
    )
}

/// The profile a pass works on, without a lease held between its steps.
///
/// A pass runs for as long as the library takes, and most of that is
/// spent waiting on the network. Holding one [`crate::state::ProfilePool`]
/// for the whole run would make every profile switch wait out the lease
/// drain timeout. So each read and each write takes a lease of its own
/// through [`AppState::require_profile_pool_for`], which refuses once the
/// profile is no longer the active one: a step can never land in another
/// profile's database, and a switch only waits for the step in flight.
#[derive(Clone)]
pub(super) struct ProfileDb {
    app: AppHandle,
    profile_id: i64,
    /// Writes wait out a running scan: true for a pass, false for a
    /// command the user is waiting on (see [`write_with`]).
    park: bool,
}

impl ProfileDb {
    /// For a background pass.
    pub(super) fn new(app: &AppHandle, profile_id: i64) -> Self {
        Self {
            app: app.clone(),
            profile_id,
            park: true,
        }
    }

    /// For a command the user is waiting on.
    pub(super) fn for_command(app: &AppHandle, profile_id: i64) -> Self {
        Self {
            park: false,
            ..Self::new(app, profile_id)
        }
    }

    /// Run `write` in a transaction on this profile, under a lease held
    /// for that write alone.
    pub(super) async fn write<T, F>(&self, write: F) -> AppResult<T>
    where
        F: for<'c> FnMut(
            &'c mut sqlx::SqliteConnection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, CoreError>> + Send + 'c>,
        >,
    {
        let lease = self.lease().await?;
        Ok(write_with(&lease, self.park, write).await?)
    }

    pub(super) async fn lease(&self) -> AppResult<crate::state::ProfilePool> {
        self.app
            .state::<AppState>()
            .require_profile_pool_for(Some(self.profile_id))
            .await
    }

    async fn fields(&self) -> AppResult<Fields> {
        Ok(read_fields(&*self.lease().await?).await)
    }
}

/// What a pass did, for the log.
#[derive(Debug, Default)]
struct PassSummary {
    applied: u32,
    review: u32,
    no_match: u32,
    failed: u32,
    covers: u32,
    artists: u32,
    cancelled: bool,
}

async fn run_pass(app: &AppHandle, db: &ProfileDb, artwork_dir: &Path) -> AppResult<()> {
    let started = Instant::now();
    let mut fields = db.fields().await?;
    let task = crate::tasks::start(
        app,
        crate::tasks::TaskKind::Enrichment,
        0,
        crate::tasks::Cancellation::Flag,
    );
    // A stop from the task bar reaches the registry's flag, not
    // `CANCEL`; it is carried over, so the guard does not answer it by
    // starting the run a scan asked for meanwhile.
    let stop = || {
        if task.as_ref().is_some_and(|t| t.is_cancelling()) {
            CANCEL.store(true, Ordering::SeqCst);
        }
        cancelled()
    };
    let (mb, caa) = clients();
    let mut summary = PassSummary::default();
    let mut last_library_emit = Instant::now();
    let mut library_dirty = false;

    // What a scan emptied since the last pass comes back first, with no
    // network at all — one album per transaction, so a library of
    // thousands never holds the writer for the whole refill.
    let lease = db.lease().await?;
    let mut conn = lease.acquire().await?;
    let applied = store::applied_album_ids(&mut conn).await?;
    drop(conn);
    drop(lease);
    for album_id in applied {
        if stop() {
            break;
        }
        let refilled = db
            .write(|conn| Box::pin(store::reapply_album(conn, album_id, fields)))
            .await?;
        library_dirty |= refilled;
    }

    let retry_before = now_ms() - NO_MATCH_RETRY_MS;
    let lease = db.lease().await?;
    let mut conn = lease.acquire().await?;
    let albums_total = store::count_pending_albums(&mut conn, retry_before).await?;
    let artists_total =
        store::count_pending_artists(&mut conn, retry_before, VARIOUS_ARTISTS_LABEL).await?;
    drop(conn);
    drop(lease);
    let mut total = (albums_total + artists_total) as u64;
    let mut done: u64 = 0;
    if let Some(task) = task.as_ref() {
        task.progress(0, total);
    }

    // ── Albums ──
    let mut cursor = 0;
    let mut failures = 0;
    'albums: loop {
        let lease = db.lease().await?;
        let mut conn = lease.acquire().await?;
        let batch = store::pending_albums(&mut conn, retry_before, cursor, BATCH).await?;
        drop(conn);
        drop(lease);
        if batch.is_empty() {
            break;
        }
        for album in batch {
            cursor = album.album_id;
            if stop() || crate::offline::is_offline() {
                summary.cancelled = true;
                break 'albums;
            }
            if let Some(task) = task.as_ref() {
                task.detail(album.title.clone());
            }
            // Read again for every album: a field the user turns off while
            // a pass runs is not written from the next album on.
            fields = db.fields().await?;
            match enrich_album(db, &mb, &caa, artwork_dir, &album, fields).await {
                Ok(outcome) => {
                    failures = 0;
                    match outcome.status {
                        Some(Status::Applied) => summary.applied += 1,
                        Some(Status::Review) => summary.review += 1,
                        _ => summary.no_match += 1,
                    }
                    summary.covers += u32::from(outcome.cover);
                    library_dirty |= outcome.changed_library;
                }
                Err(err) => {
                    summary.failed += 1;
                    failures += 1;
                    tracing::warn!(album_id = album.album_id, %err, "enrichment lookup failed");
                    if failures >= MAX_CONSECUTIVE_FAILURES {
                        tracing::warn!("musicbrainz keeps failing; ending the pass");
                        break 'albums;
                    }
                }
            }
            done += 1;
            if let Some(task) = task.as_ref() {
                task.progress(done, total);
            }
            let _ = app.emit(CHANGED_EVENT, ());
            if library_dirty && last_library_emit.elapsed() >= LIBRARY_EMIT_GAP {
                let _ = app.emit("library:rescanned", ());
                last_library_emit = Instant::now();
                library_dirty = false;
            }
        }
    }

    // ── Covers a previous pass could not fetch ──
    // Read after the albums, not before: the albums this pass applied
    // fetched their own, and listing them first would download each
    // twice.
    fields = db.fields().await?;
    if fields.covers && !summary.cancelled && failures < MAX_CONSECUTIVE_FAILURES {
        let lease = db.lease().await?;
        let mut conn = lease.acquire().await?;
        let backlog = store::albums_wanting_cover(&mut conn).await?;
        drop(conn);
        drop(lease);
        total += backlog.len() as u64;
        for (album_id, release, group, has_front) in backlog {
            if stop() || crate::offline::is_offline() {
                summary.cancelled = true;
                break;
            }
            match fetch_cover(
                db,
                &caa,
                artwork_dir,
                album_id,
                &release,
                group.as_deref(),
                has_front,
            )
            .await
            {
                Ok(true) => {
                    summary.covers += 1;
                    library_dirty = true;
                }
                Ok(false) => {}
                Err(err) => tracing::warn!(album_id, %err, "cover fetch failed"),
            }
            done += 1;
            if let Some(task) = task.as_ref() {
                task.progress(done, total);
            }
        }
    }

    // ── Artists no matched album named ──
    if !summary.cancelled && failures < MAX_CONSECUTIVE_FAILURES {
        let mut cursor = 0;
        'artists: loop {
            let lease = db.lease().await?;
            let mut conn = lease.acquire().await?;
            let batch = store::pending_artists(
                &mut conn,
                retry_before,
                VARIOUS_ARTISTS_LABEL,
                cursor,
                BATCH,
            )
            .await?;
            drop(conn);
            drop(lease);
            if batch.is_empty() {
                break;
            }
            for (artist_id, name) in batch {
                cursor = artist_id;
                if stop() || crate::offline::is_offline() {
                    summary.cancelled = true;
                    break 'artists;
                }
                if let Some(task) = task.as_ref() {
                    task.detail(name.clone());
                }
                match resolve::resolve_artist(&mb, &name).await {
                    Ok(found) => {
                        failures = 0;
                        let now = now_ms();
                        db.write(|conn| {
                            let found = found.clone();
                            Box::pin(async move {
                                match found {
                                    Some(p) => {
                                        store::propose_artist(conn, artist_id, &p, now).await
                                    }
                                    None => {
                                        store::record_artist_no_match(conn, artist_id, now).await
                                    }
                                }
                            })
                        })
                        .await?;
                        summary.artists += 1;
                    }
                    // As for an album: a refusal that is this artist's is
                    // remembered as no match rather than counted.
                    Err(err) if is_permanent(&err) => {
                        failures = 0;
                        tracing::warn!(artist_id, %err, "artist lookup refused; no match");
                        let now = now_ms();
                        db.write(|conn| {
                            Box::pin(store::record_artist_no_match(conn, artist_id, now))
                        })
                        .await?;
                    }
                    Err(err) => {
                        failures += 1;
                        tracing::warn!(artist_id, %err, "artist lookup failed");
                        if failures >= MAX_CONSECUTIVE_FAILURES {
                            break 'artists;
                        }
                    }
                }
                done += 1;
                if let Some(task) = task.as_ref() {
                    task.progress(done, total);
                }
                let _ = app.emit(CHANGED_EVENT, ());
            }
        }
    }

    if library_dirty {
        let _ = app.emit("library:rescanned", ());
    }
    tracing::info!(
        applied = summary.applied,
        review = summary.review,
        no_match = summary.no_match,
        failed = summary.failed,
        covers = summary.covers,
        artists = summary.artists,
        cancelled = summary.cancelled,
        secs = started.elapsed().as_secs(),
        "enrichment pass finished"
    );
    Ok(())
}

/// What one album came to.
pub(super) struct AlbumOutcome {
    /// `None` for no match.
    pub status: Option<Status>,
    pub cover: bool,
    pub changed_library: bool,
}

/// Resolve one album and write the decision.
async fn enrich_album(
    db: &ProfileDb,
    mb: &MusicBrainzClient,
    caa: &CoverArtArchiveClient,
    artwork_dir: &Path,
    album: &LocalAlbum,
    fields: Fields,
) -> AppResult<AlbumOutcome> {
    // The files' own identifier, from the first track: a tagger writes
    // the same release into every file of an album.
    let first = album.tracks.first().map(|t| PathBuf::from(&t.file_path));
    let tagged = match first {
        Some(path) => tokio::task::spawn_blocking(move || tags::release_mbid(&path))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let now = now_ms();
    let resolution = match resolve::resolve_album(mb, album, tagged.as_deref()).await {
        Ok(found) => found,
        // An answer that will be the same next time and is this album's
        // (`is_permanent`) is remembered as no match, asked again in a
        // month, so it neither counts toward the failures that end a pass
        // nor stays first in the queue of every pass after.
        Err(err) if is_permanent(&err) => {
            tracing::warn!(album_id = album.album_id, %err, "enrichment lookup refused; no match");
            None
        }
        Err(err) => return Err(crate::error::AppError::Other(format!("musicbrainz: {err}"))),
    };
    let Some(resolution) = resolution else {
        db.write(|conn| Box::pin(store::record_no_match(conn, album.album_id, now)))
            .await?;
        return Ok(AlbumOutcome {
            status: None,
            cover: false,
            changed_library: false,
        });
    };
    let status = match resolution.verdict {
        Verdict::Confident => Status::Applied,
        Verdict::Doubtful => Status::Review,
        // `resolve_album` keeps no such resolution; were one to come
        // back, it is what it says.
        Verdict::NoMatch => {
            db.write(|conn| Box::pin(store::record_no_match(conn, album.album_id, now)))
                .await?;
            return Ok(AlbumOutcome {
                status: None,
                cover: false,
                changed_library: false,
            });
        }
    };
    apply_resolution(
        db,
        caa,
        artwork_dir,
        album.album_id,
        &resolution,
        status,
        fields,
    )
    .await
}

/// Write a resolution as `status`, propose the artists it credits when it
/// is applied, and fetch the cover it allows.
pub(super) async fn apply_resolution(
    db: &ProfileDb,
    caa: &CoverArtArchiveClient,
    artwork_dir: &Path,
    album_id: i64,
    resolution: &Resolution,
    status: Status,
    fields: Fields,
) -> AppResult<AlbumOutcome> {
    let now = now_ms();
    let report: ApplyReport = db
        .write(|conn| {
            // Owned by each attempt: the future outlives no borrow of the
            // caller's, which is what a retried transaction needs.
            let resolution = resolution.clone();
            Box::pin(async move {
                let resolution = &resolution;
                let report = store::record_match(
                    conn,
                    album_id,
                    status,
                    resolution.score.total,
                    &resolution.facts,
                    &resolution.tracks,
                    fields,
                    now,
                )
                .await?;
                if status == Status::Applied {
                    propose_credited_artists(conn, album_id, resolution, now).await?;
                }
                Ok(report)
            })
        })
        .await?;
    let mut cover = false;
    if report.wants_cover {
        match fetch_cover(
            db,
            caa,
            artwork_dir,
            album_id,
            &resolution.facts.release_mbid,
            resolution.facts.release_group_mbid.as_deref(),
            resolution.facts.has_front_cover,
        )
        .await
        {
            Ok(linked) => cover = linked,
            // Picked up by the next pass's cover backlog when the release
            // says it has one.
            Err(err) => tracing::warn!(album_id, %err, "cover fetch failed"),
        }
    }
    Ok(AlbumOutcome {
        status: Some(status),
        cover,
        changed_library: cover
            || report.year
            || report.release_date
            || report.track_years > 0
            || report.genre_tracks > 0,
    })
}

/// Propose, for review, the MusicBrainz artist behind each library
/// artist this applied album credits under the same name.
async fn propose_credited_artists(
    conn: &mut sqlx::SqliteConnection,
    album_id: i64,
    resolution: &Resolution,
    now: i64,
) -> Result<(), CoreError> {
    let locals = store::album_artists(conn, album_id).await?;
    for (credited, artist) in &resolution.credited {
        if artist.id == VARIOUS_ARTISTS_MBID {
            continue;
        }
        for (artist_id, local_name) in &locals {
            if local_name == VARIOUS_ARTISTS_LABEL {
                continue;
            }
            if names_match(local_name, credited) || names_match(local_name, &artist.name) {
                let proposal = ArtistProposal {
                    mbid: artist.id.clone(),
                    name: artist.name.clone(),
                    disambiguation: artist.disambiguation.clone().filter(|d| !d.is_empty()),
                    kind: artist.kind.clone(),
                    country: artist.country.clone(),
                    score: resolution.score.total,
                    album_evidence: 1,
                };
                store::propose_artist(conn, *artist_id, &proposal, now).await?;
            }
        }
    }
    Ok(())
}

/// Fetch a front cover and link it, if the album still has none. `true`
/// when a cover was linked.
///
/// The release's own cover when the archive says it has one, else the
/// release group's — the one the archive chose for the album across its
/// pressings.
pub(super) async fn fetch_cover(
    db: &ProfileDb,
    caa: &CoverArtArchiveClient,
    artwork_dir: &Path,
    album_id: i64,
    release: &str,
    group: Option<&str>,
    has_front: bool,
) -> AppResult<bool> {
    if crate::offline::is_offline() {
        return Ok(false);
    }
    let bytes = if has_front {
        caa.front(CoverOf::Release, release).await
    } else if let Some(group) = group {
        caa.front(CoverOf::ReleaseGroup, group).await
    } else {
        Ok(None)
    }
    .map_err(|e| crate::error::AppError::Other(format!("cover art archive: {e}")))?;
    let Some(bytes) = bytes else {
        return Ok(false);
    };
    let Some(format) = waveflow_core::artwork::image_format::detect_image_format(&bytes) else {
        return Ok(false);
    };
    // Hashing up to 16 MiB and writing it is blocking work: off the
    // runtime, as the scanner does its own.
    let dir = artwork_dir.to_path_buf();
    let (hash, target) = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        std::fs::create_dir_all(&dir)?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let target = dir.join(format!("{hash}.{format}"));
        waveflow_core::atomic_write::store_content_addressed(&target, &bytes)?;
        Ok((hash, target))
    })
    .await
    .map_err(|e| crate::error::AppError::Other(format!("cover write task failed: {e}")))??;
    let linked = db
        .write(|conn| {
            let hash = hash.clone();
            Box::pin(async move {
                // Checked in the transaction that links, so an album that
                // found a cover of its own meanwhile (a scan, the user)
                // gets no artwork row pointing at nothing.
                let bare: Option<bool> =
                    sqlx::query_scalar("SELECT artwork_id IS NULL FROM album WHERE id = ?")
                        .bind(album_id)
                        .fetch_optional(&mut *conn)
                        .await?;
                if bare != Some(true) {
                    return Ok(false);
                }
                // `manual`: `artwork.source`'s CHECK knows no other online
                // source, and the album link carries the real provenance
                // (`store::COVER_SOURCE`).
                let artwork_id = upsert_artwork(conn, &hash, format, ArtworkSource::Manual).await?;
                store::link_cover(conn, album_id, artwork_id).await
            })
        })
        .await?;
    if linked {
        crate::thumbnails::spawn_thumbnail_job(target, artwork_dir.to_path_buf(), hash);
    }
    Ok(linked)
}
