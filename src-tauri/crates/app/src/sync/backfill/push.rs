//! Backfill push direction — re-emit local rows the server's
//! digest doesn't see (Phase B.2 / RFC-003 §4).
//!
//! For each `canonical_id` flagged `missing_remotely`, read the
//! local row's canonical fields and enqueue an `insert` op via
//! the regular outbox path. The drain task picks it up on the
//! next pass (the orchestrator wakes the drain after a non-zero
//! push count so the user doesn't wait the 30 s tick).
//!
//! ## HLC clobber guard
//!
//! Re-emitting via [`crate::sync::hooks::enqueue_op_in_tx`] draws
//! a fresh HLC from [`crate::sync::hlc::next_conn`], so the
//! server side sees the row as a "now" write rather than a
//! replay of when the user actually edited. That's intentional —
//! the original HLC is lost on the queue side anyway; what
//! matters for §2 LWW resolution at the server is that the
//! re-emit doesn't go backwards. Since we always draw a fresh
//! monotonic HLC, this property holds by construction.
//!
use serde_json::{Map, Value};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::sync::digest::client::RemoteMaxHlc;
use crate::sync::digest::diff::DivergentMember;
use crate::sync::hooks::{self, EnqueuedStamp, PendingOpDraft};
use crate::sync::payload;

/// Counters returned to the orchestrator.
#[derive(Debug, Default)]
pub struct PushStats {
    pub pushed: u32,
    /// Reserved for a future HLC-guard implementation. Stays 0
    /// in the current always-push policy (see module banner).
    pub skipped_out_of_date: u32,
    pub failed: u32,
}

/// Push every member of `missing_remotely` for the given entity.
/// `_remote_max_hlc` is plumbed in for a future guard — see the
/// module banner.
pub async fn push_missing_remotely(
    state: &AppState,
    pool: &SqlitePool,
    entity: &str,
    missing: &[DivergentMember],
    _remote_max_hlc: Option<RemoteMaxHlc>,
) -> AppResult<PushStats> {
    let mut stats = PushStats::default();

    for member in missing {
        match push_one_by_canonical(state, pool, entity, &member.canonical_id).await {
            Ok(true) => stats.pushed += 1,
            Ok(false) => {
                tracing::debug!(
                    entity,
                    canonical_id = %member.canonical_id,
                    "backfill push: local row gone, skipping"
                );
            }
            Err(err) => {
                tracing::warn!(
                    entity,
                    canonical_id = %member.canonical_id,
                    error = %err,
                    "backfill push failed for row"
                );
                stats.failed += 1;
            }
        }
    }
    Ok(stats)
}

/// Push one row by canonical_id. Pub(super) so [`super::lww`]
/// reuses it on the "local wins" branch.
pub(super) async fn push_one_by_canonical(
    state: &AppState,
    pool: &SqlitePool,
    entity: &str,
    canonical_id: &str,
) -> AppResult<bool> {
    match entity {
        "library" => push_library(state, pool, canonical_id).await,
        "playlist" => push_playlist(state, pool, canonical_id).await,
        "track" => push_track(state, pool, canonical_id).await,
        "liked_track" => push_liked_track(state, pool, canonical_id).await,
        "track_rating" => push_track_rating(state, pool, canonical_id).await,
        other => Err(AppError::Other(format!(
            "push_one_by_canonical: unsupported entity '{other}'",
        ))),
    }
}

// ── Per-entity push implementations ───────────────────────────

/// Push a `library` row. Reads canonical fields via the existing
/// B.0a helper and enqueues an `insert` op against the row's
/// canonical id. Returns `Ok(true)` when the op was enqueued,
/// `Ok(false)` when the local row is gone.
async fn push_library(state: &AppState, pool: &SqlitePool, canonical_id: &str) -> AppResult<bool> {
    let mut tx = pool.begin().await?;
    let local_id: Option<i64> = sqlx::query_scalar("SELECT id FROM library WHERE canonical_id = ?")
        .bind(canonical_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(local_id) = local_id else {
        return Ok(false);
    };
    let Some(fields) = payload::library::fields_from_row(&mut tx, local_id).await? else {
        return Ok(false);
    };
    enqueue_insert(
        &mut tx,
        "library",
        canonical_id,
        fields,
        local_id,
        Some(stamp_library),
    )
    .await?;
    tx.commit().await?;
    let _ = state;
    Ok(true)
}

async fn push_playlist(state: &AppState, pool: &SqlitePool, canonical_id: &str) -> AppResult<bool> {
    let mut tx = pool.begin().await?;
    let local_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM playlist WHERE canonical_id = ?")
            .bind(canonical_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(local_id) = local_id else {
        return Ok(false);
    };
    let Some(fields) = payload::playlist::fields_from_row(&mut tx, local_id).await? else {
        return Ok(false);
    };
    enqueue_insert(
        &mut tx,
        "playlist",
        canonical_id,
        fields,
        local_id,
        Some(stamp_playlist),
    )
    .await?;
    tx.commit().await?;
    let _ = state;
    Ok(true)
}

/// `liked_track` canonical is the file_hash. The wire payload is
/// the empty map `{}`; the canonical_fields hash agrees.
async fn push_liked_track(state: &AppState, pool: &SqlitePool, file_hash: &str) -> AppResult<bool> {
    let mut tx = pool.begin().await?;
    let track_id: Option<i64> = sqlx::query_scalar(
        "SELECT t.id FROM liked_track lt
            JOIN track t ON t.id = lt.track_id
           WHERE t.file_hash = ?",
    )
    .bind(file_hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(track_id) = track_id else {
        return Ok(false);
    };
    let fields = payload::liked_track::canonical_fields();
    let stamp = hooks::enqueue_op_in_tx(
        &mut tx,
        &PendingOpDraft {
            entity: "liked_track".into(),
            entity_id: file_hash.to_owned(),
            field: None,
            op: "insert".into(),
            payload: Some(Value::Object(fields)),
        },
    )
    .await?;
    if let Some(stamp) = stamp {
        payload::liked_track::stamp_in_tx(&mut tx, track_id, stamp).await?;
    }
    tx.commit().await?;
    let _ = state;
    Ok(true)
}

/// Push a `track` row. The canonical id is the composite
/// `<library.canonical_id>\u{1F}<track.file_path>` shipped by the
/// digest endpoint. We split it on `U+001F`, look up the local
/// row, rebuild the [`TrackInsertWire`] from its joined state +
/// `track_artist` list, and re-enqueue via
/// [`crate::sync::track_emit::emit_track_insert_in_tx`] — the
/// same helper the scanner uses for fresh imports + tag-edit
/// re-emits, so the wire shape stays byte-identical regardless
/// of which path triggered the push.
///
/// Returns `Ok(false)` when the local row vanished between the
/// digest read and the push (concurrent delete) or when the
/// composite canonical is malformed.
async fn push_track(state: &AppState, pool: &SqlitePool, canonical_id: &str) -> AppResult<bool> {
    let Some((lib_canonical, file_path)) = canonical_id.split_once('\u{001F}') else {
        return Err(AppError::Other(format!(
            "push_track: composite canonical must contain `\\u{{001F}}`, got `{canonical_id}`",
        )));
    };
    if lib_canonical.is_empty() || file_path.is_empty() {
        return Err(AppError::Other(
            "push_track: composite canonical halves must be non-empty".into(),
        ));
    }

    let mut tx = pool.begin().await?;
    let row: Option<TrackPushRow> = sqlx::query_as::<_, TrackPushRow>(
        "SELECT \
            t.id, t.library_id, t.file_path, t.file_hash, \
            t.file_size, t.file_modified, t.duration_ms, t.title, \
            t.track_number, t.disc_number, t.year, t.bitrate, \
            t.sample_rate, t.channels, t.bit_depth, t.codec, \
            t.musical_key, t.added_at, \
            al.title AS album_title, \
            al.album_artist AS album_artist_name, \
            COALESCE(al.is_compilation, 0) AS is_compilation \
           FROM track t \
           JOIN library l ON l.id = t.library_id \
      LEFT JOIN album al ON al.id = t.album_id \
          WHERE l.canonical_id = ? AND t.file_path = ?",
    )
    .bind(lib_canonical)
    .bind(file_path)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };

    // Multi-artist list, ordered by `track_artist.position` so the
    // wire's array index matches the server's `position` column —
    // critical because the canonical `artists: [String]` hash
    // preserves source order.
    let artists: Vec<String> = sqlx::query_scalar(
        "SELECT a.name FROM track_artist ta \
            JOIN artist a ON a.id = ta.artist_id \
           WHERE ta.track_id = ? \
           ORDER BY ta.position ASC",
    )
    .bind(row.id)
    .fetch_all(&mut *tx)
    .await?;

    let wire = crate::sync::track_emit::TrackInsertWire {
        file_hash: &row.file_hash,
        title: &row.title,
        file_size: row.file_size,
        file_modified: row.file_modified,
        duration_ms: row.duration_ms,
        track_number: row.track_number,
        disc_number: row.disc_number,
        year: row.year,
        bitrate: row.bitrate,
        sample_rate: row.sample_rate,
        channels: row.channels,
        bit_depth: row.bit_depth,
        codec: row.codec.as_deref(),
        musical_key: row.musical_key.as_deref(),
        added_at: row.added_at,
        album_title: row.album_title.as_deref(),
        album_artist_name: row.album_artist_name.as_deref(),
        is_compilation: row.is_compilation != 0,
        artists: &artists,
    };
    crate::sync::track_emit::emit_track_insert_in_tx(
        &mut tx,
        row.library_id,
        row.id,
        file_path,
        &wire,
    )
    .await?;
    tx.commit().await?;
    let _ = state;
    Ok(true)
}

/// Projection mirroring `commands/scan.rs`'s track row shape +
/// the album fields the canonical_fields map needs.
#[derive(sqlx::FromRow)]
struct TrackPushRow {
    id: i64,
    library_id: i64,
    #[allow(dead_code)]
    file_path: String,
    file_hash: String,
    file_size: i64,
    file_modified: i64,
    duration_ms: i64,
    title: String,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    year: Option<i64>,
    bitrate: Option<i64>,
    sample_rate: Option<i64>,
    channels: Option<i64>,
    bit_depth: Option<i64>,
    codec: Option<String>,
    musical_key: Option<String>,
    added_at: i64,
    album_title: Option<String>,
    album_artist_name: Option<String>,
    is_compilation: i64,
}

/// `track_rating` canonical is also the file_hash. The wire
/// payload uses `{value: <0..=255>}` to match the apply pipeline's
/// existing shape; the canonical_fields used for the hash use
/// `{rating: <0..=255>}` (server's apply canonical for
/// track_rating).
async fn push_track_rating(
    state: &AppState,
    pool: &SqlitePool,
    file_hash: &str,
) -> AppResult<bool> {
    let mut tx = pool.begin().await?;
    let row: Option<(i64, Option<i64>)> =
        sqlx::query_as("SELECT id, rating FROM track WHERE file_hash = ?")
            .bind(file_hash)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((track_id, rating)) = row else {
        return Ok(false);
    };
    let Some(rating) = rating else {
        // No rating to push — the digest membership shouldn't
        // have flagged this row, but stay defensive.
        return Ok(false);
    };
    let stamp = hooks::enqueue_op_in_tx(
        &mut tx,
        &PendingOpDraft {
            entity: "track_rating".into(),
            entity_id: file_hash.to_owned(),
            field: None,
            op: "set".into(),
            payload: Some(serde_json::json!({ "value": rating })),
        },
    )
    .await?;
    if let Some(stamp) = stamp {
        // stamp_set_in_tx rebuilds the canonical fields internally
        // from `rating` — no need to compute them here.
        payload::track_rating::stamp_set_in_tx(&mut tx, track_id, rating, stamp).await?;
    }
    tx.commit().await?;
    let _ = state;
    Ok(true)
}

// ── Shared insert helper for library / playlist ───────────────

type StampFn = fn(
    &mut SqliteConnection,
    i64,
    Map<String, Value>,
    EnqueuedStamp,
) -> futures::future::BoxFuture<'_, AppResult<()>>;

/// Build + enqueue an `insert` op whose payload is the canonical
/// fields map, then stamp the row's HLC + payload_hash if the
/// queue write produced a stamp.
async fn enqueue_insert(
    conn: &mut SqliteConnection,
    entity: &str,
    canonical_id: &str,
    fields: Map<String, Value>,
    local_id: i64,
    stamp_fn: Option<StampFn>,
) -> AppResult<()> {
    let stamp = hooks::enqueue_op_in_tx(
        conn,
        &PendingOpDraft {
            entity: entity.to_owned(),
            entity_id: canonical_id.to_owned(),
            field: None,
            op: "insert".into(),
            payload: Some(Value::Object(fields.clone())),
        },
    )
    .await?;
    if let (Some(stamp), Some(stamp_fn)) = (stamp, stamp_fn) {
        stamp_fn(conn, local_id, fields, stamp).await?;
    }
    Ok(())
}

fn stamp_library(
    conn: &mut SqliteConnection,
    local_id: i64,
    fields: Map<String, Value>,
    stamp: EnqueuedStamp,
) -> futures::future::BoxFuture<'_, AppResult<()>> {
    Box::pin(async move { payload::library::stamp_in_tx(conn, local_id, fields, stamp).await })
}

fn stamp_playlist(
    conn: &mut SqliteConnection,
    local_id: i64,
    fields: Map<String, Value>,
    stamp: EnqueuedStamp,
) -> futures::future::BoxFuture<'_, AppResult<()>> {
    Box::pin(async move { payload::playlist::stamp_in_tx(conn, local_id, fields, stamp).await })
}
