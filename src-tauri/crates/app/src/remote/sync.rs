//! Bootstrap and incremental reads against the journal.
//!
//! ```text
//! BOOTSTRAP  GET /sync/snapshot ──▶ atomic replace ──▶ cursor ──▶ ACK
//! CATCH UP   GET /sync/changes?after=cursor&limit=… ──▶ apply ──▶ cursor ──▶ ACK
//!            while has_more, immediately ask for the next page
//! ```
//!
//! ## Everything here is driven by the durable cursor
//!
//! Not by what arrives, and not by the socket. A page is applied and the
//! cursor advanced **in one transaction**, so a crash between the two is
//! impossible: either the page landed and the position moved, or neither
//! did and the same page is fetched again. Applying a page twice is
//! harmless — every write in [`super::projection`] is an upsert or a
//! delete keyed by identity.
//!
//! ## Recovery is a snapshot, never a repair
//!
//! An event this build does not understand is skipped and the cursor
//! keeps moving. A **known** event that fails to apply means the
//! projection no longer reflects the journal, and the only sound answer
//! is to throw it away and take a fresh snapshot. [`catch_up`] does that
//! once; if the snapshot itself cannot be applied, the error is real and
//! propagates.
//!
//! ## The acknowledgement never blocks anything
//!
//! It feeds observability and future retention, not read authorization.
//! A failing ACK is logged and forgotten — treating it as fatal would
//! stall synchronization over a signal the protocol explicitly says is
//! not a prerequisite.

use crate::{
    error::{AppError, AppResult},
    remote::{
        binding::{self, RemoteBinding},
        client::RemoteClient,
        dto::{SongItem, SyncPage, SyncSnapshot},
        projection::{self, Outcome},
    },
    state::AppState,
};

/// Page size for `/sync/changes`. The server caps it at 500; asking for
/// the maximum keeps a long catch-up to as few round-trips as possible,
/// and a page is applied in one transaction either way.
const PAGE_LIMIT: i64 = 500;

/// How many pages a single catch-up will walk before yielding.
///
/// Not a correctness bound — the loop stops on `has_more` — but a
/// runaway guard: a server that always answered `has_more` with the same
/// cursor would otherwise spin forever holding a lease. Hitting it is
/// logged, never silent.
const MAX_PAGES: usize = 10_000;

/// How many missing tracks to fetch after a catch-up. Metadata is a
/// display concern, so it is filled opportunistically rather than
/// blocking the cursor.
const TRACK_BACKFILL_LIMIT: usize = 200;

/// What a synchronization pass did, for the UI and the logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncReport {
    pub applied: usize,
    pub ignored: usize,
    pub pages: usize,
    pub cursor: i64,
    /// Whether the pass had to fall back to a fresh snapshot.
    pub resnapshotted: bool,
}

/// Bring the projection up to date, bootstrapping first if it has never
/// been.
///
/// The entry point for every trigger: sign-in, app start, the socket
/// wake-up and the periodic pass all call this.
pub async fn sync_now(state: &AppState) -> AppResult<SyncReport> {
    let Some(client) = RemoteClient::try_build(state).await? else {
        // Not bound, signed out, or bound to a server with no journal.
        // All three are ordinary states, not failures.
        return Ok(SyncReport::default());
    };

    let profile_id = state.require_profile_id().await?;

    // Push before pulling, for two reasons. Our own changes then come
    // back in the same pass instead of the next one — and, more
    // importantly, a bootstrap replaces the projection wholesale, so
    // anything still queued should have had its chance to become server
    // state first. A failure here is not fatal: the queue keeps its
    // entries and the read half still runs.
    if let Err(error) = crate::remote::drain::drain(state).await {
        tracing::debug!(%error, "could not push the outbound queue before syncing");
    }

    let binding = read_binding(state, profile_id).await?;

    match binding.bootstrapped_at {
        None => {
            let cursor = bootstrap(state, profile_id, &client).await?;
            Ok(SyncReport {
                cursor,
                resnapshotted: true,
                ..Default::default()
            })
        }
        Some(_) => catch_up(state, profile_id, &client, binding.identity.cursor().unwrap_or(0)).await,
    }
}

async fn read_binding(state: &AppState, profile_id: i64) -> AppResult<RemoteBinding> {
    let pool = state.require_profile_pool_for(Some(profile_id)).await?;
    let mut conn = pool.acquire().await?;
    binding::read(&mut conn)
        .await?
        .ok_or_else(|| AppError::Other("this profile is not bound to a server".into()))
}

/// Replace the whole projection with one writer-consistent snapshot.
///
/// The fetch happens outside the transaction and the write inside it, so
/// a slow download never holds the database open, and the projection is
/// never observable half-replaced.
pub async fn bootstrap(
    state: &AppState,
    profile_id: i64,
    client: &RemoteClient,
) -> AppResult<i64> {
    let snapshot: SyncSnapshot = client
        .send_json(client.get("/api/v2/sync/snapshot"))
        .await
        .map_err(|failure| AppError::Other(format!("could not fetch the snapshot: {failure}")))?;

    let cursor = snapshot.cursor;
    {
        let pool = state.require_profile_pool_for(Some(profile_id)).await?;
        let mut tx = pool.begin().await?;
        projection::apply_snapshot(&mut tx, &snapshot).await?;
        // Pinned, not `MAX`-ed: a snapshot is authoritative even when its
        // cursor is lower than a position reached before the projection
        // was discarded — which is exactly the recovery case.
        binding::mark_bootstrapped(&mut tx, cursor).await?;
        tx.commit().await?;
    }

    acknowledge(state, profile_id, client, cursor).await;
    Ok(cursor)
}

/// Walk the journal from `after` to its end.
pub async fn catch_up(
    state: &AppState,
    profile_id: i64,
    client: &RemoteClient,
    after: i64,
) -> AppResult<SyncReport> {
    let mut report = SyncReport {
        cursor: after,
        ..Default::default()
    };

    loop {
        if report.pages >= MAX_PAGES {
            tracing::warn!(
                cursor = report.cursor,
                pages = report.pages,
                "stopping a catch-up that never reported the end of the journal"
            );
            break;
        }

        let page: SyncPage = match client
            .send_json(client.get(&format!(
                "/api/v2/sync/changes?after={}&limit={PAGE_LIMIT}",
                report.cursor
            )))
            .await
        {
            Ok(page) => page,
            // The journal no longer reaches back this far — a future
            // compaction dropped the events between our cursor and the
            // surviving floor. Nothing local can fill that gap, so the
            // projection is discarded and rebuilt from a snapshot.
            //
            // Tested on the *code*, never the status: `409` also carries
            // `conflict`, and mistaking that for this would throw away a
            // perfectly healthy projection over a rejected write.
            Err(failure) if failure.is_cursor_expired() => {
                tracing::info!(
                    cursor = report.cursor,
                    "the journal no longer reaches this cursor; taking a fresh snapshot"
                );
                let cursor = bootstrap(state, profile_id, client).await?;
                return Ok(SyncReport {
                    cursor,
                    resnapshotted: true,
                    pages: report.pages + 1,
                    ..Default::default()
                });
            }
            Err(failure) => {
                return Err(AppError::Other(format!(
                    "could not fetch changes: {failure}"
                )))
            }
        };

        report.pages += 1;
        let has_more = page.has_more;
        let next_cursor = page.next_cursor;

        let pool = state.require_profile_pool_for(Some(profile_id)).await?;
        let mut tx = pool.begin().await?;

        let mut failed = false;
        for change in &page.changes {
            match projection::apply_change(&mut tx, change).await {
                Ok(Outcome::Applied) => report.applied += 1,
                Ok(Outcome::Ignored) => report.ignored += 1,
                Err(error) => {
                    // A known event we could not apply. The projection can
                    // no longer be trusted, so nothing from this page is
                    // kept and the cursor does not move.
                    tracing::warn!(
                        cursor = change.cursor,
                        entity = %change.entity_type,
                        action = %change.action,
                        error = %error,
                        "a known event could not be applied; taking a fresh snapshot"
                    );
                    failed = true;
                    break;
                }
            }
        }

        if failed {
            tx.rollback().await?;
            let cursor = bootstrap(state, profile_id, client).await?;
            return Ok(SyncReport {
                cursor,
                resnapshotted: true,
                pages: report.pages,
                ..Default::default()
            });
        }

        // The page and the cursor commit together. A crash here either
        // keeps both or loses both, so the same page is simply fetched
        // again — and re-applying it is harmless.
        binding::advance_cursor(&mut tx, next_cursor).await?;
        tx.commit().await?;
        report.cursor = next_cursor;

        if !has_more {
            break;
        }
    }

    acknowledge(state, profile_id, client, report.cursor).await;
    backfill_missing_tracks(state, profile_id, client).await;
    Ok(report)
}

/// Tell the server how far this device has got.
///
/// Deliberately infallible from the caller's point of view: a failed ACK
/// is an observability gap, never a reason to stop synchronizing.
async fn acknowledge(state: &AppState, profile_id: i64, client: &RemoteClient, cursor: i64) {
    let Ok(binding) = read_binding(state, profile_id).await else {
        return;
    };
    let Some(device_id) = binding.identity.device_id() else {
        return;
    };

    // A plain request, not a `mutate`: the acknowledgement is not a
    // journalled mutation, so it carries no operation identifier. Minting
    // a fresh one on every pass would be worse than useless — it would
    // suggest an idempotency the endpoint does not implement.
    let outcome = client
        .send_ok(
            client
                .request(reqwest::Method::PUT, "/api/v2/sync/ack")
                .json(&serde_json::json!({ "device_id": device_id, "cursor": cursor })),
        )
        .await;

    if let Err(failure) = outcome {
        // A `422` here confounds two causes: a cursor beyond the user's
        // latest event, and a device the server does not know or has
        // revoked. The second is far more likely and far more
        // consequential — a revoked device fails every *mutation* too —
        // so the log names it first. (The server side hit this exact
        // confusion while testing: an empty device id reads as a bad
        // cursor.)
        if failure.status == Some(422) {
            tracing::warn!(
                %failure,
                cursor,
                device_id,
                "the server refused this acknowledgement; check the device is still \
                 registered before suspecting the cursor"
            );
        } else {
            tracing::debug!(%failure, cursor, "acknowledgement failed; continuing anyway");
        }
    }
}

/// Fetch metadata for identifiers the journal referenced but never
/// described.
///
/// Best-effort by design. The projection is already correct without it —
/// ordering and identity are stored — and a failure here costs a
/// placeholder row, not a wrong one.
async fn backfill_missing_tracks(state: &AppState, profile_id: i64, client: &RemoteClient) {
    let missing = {
        let Ok(pool) = state.require_profile_pool_for(Some(profile_id)).await else {
            return;
        };
        let Ok(mut conn) = pool.acquire().await else {
            return;
        };
        match projection::missing_track_ids(&mut conn).await {
            Ok(ids) => ids,
            Err(error) => {
                tracing::debug!(%error, "could not list tracks awaiting metadata");
                return;
            }
        }
    };
    if missing.is_empty() {
        return;
    }

    let total = missing.len();
    let mut fetched = Vec::new();
    for id in missing.into_iter().take(TRACK_BACKFILL_LIMIT) {
        match client
            .send_json::<SongItem>(client.get(&format!("/api/v2/tracks/{id}")))
            .await
        {
            Ok(song) => fetched.push(song),
            // A track the account can no longer see is not an error: it
            // stays a placeholder, which is the honest rendering.
            Err(failure) => tracing::debug!(%failure, track = %id, "track metadata unavailable"),
        }
    }

    if total > TRACK_BACKFILL_LIMIT {
        tracing::info!(
            total,
            fetched = fetched.len(),
            "more tracks await metadata than one pass fetches; the rest follow next pass"
        );
    }

    let Ok(pool) = state.require_profile_pool_for(Some(profile_id)).await else {
        return;
    };
    let Ok(mut tx) = pool.begin().await else { return };
    for song in &fetched {
        if let Err(error) = projection::cache_song(&mut tx, song).await {
            tracing::debug!(%error, "could not cache track metadata");
            return;
        }
    }
    let _ = tx.commit().await;
}
