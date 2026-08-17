//! The outbound queue: local changes on their way to the server.
//!
//! Not generic operations but **replayable business calls**. Each row is
//! one REST request the drain re-issues verbatim under its
//! `operation_id`, which the server reads from `X-WaveFlow-Operation-Id`
//! to recognize a replay instead of applying it twice.
//!
//! ## An entry is immutable once queued
//!
//! The server fingerprints the action, the target and the normalized
//! payload behind the operation identifier. Presenting the same
//! identifier with a different fingerprint is *rejected as a conflict*,
//! not accepted as a replay. So correcting a gesture before the queue
//! drains — renaming a playlist that is already queued — means enqueuing
//! a **second** mutation with a new identifier, or merging the two
//! before enqueueing and minting a fresh one. Editing `payload` in place
//! while keeping `operation_id` is the one thing this module must never
//! do.
//!
//! The single exception is [`resolve_placeholder`], and it is safe for a
//! reason worth stating: it only rewrites entries that reference a
//! playlist which does not exist on the server yet, and FIFO order
//! guarantees those have never been sent.
//!
//! ## Ordering, and why a transient failure stops the pass
//!
//! Strict FIFO. An update must never overtake the creation it depends
//! on, so the first transient failure ends the pass and everything
//! behind it waits. A *permanent* failure is different: it can never
//! succeed, so the row is marked and the drain moves on rather than
//! deadlocking the queue behind it.
//!
//! ## Creating something that has no server identifier yet
//!
//! A playlist created offline cannot know its server identifier. It gets
//! a local placeholder — `local:<uuid>` — which the projection uses as a
//! real key, so the playlist is visible and editable immediately. When
//! the creation finally lands, [`resolve_placeholder`] rewrites the
//! projection and any still-queued mutation that named it.

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};
use uuid::Uuid;

use crate::error::AppResult;

/// Prefix marking an identifier this device invented while offline.
/// Server identifiers are UUIDs, so the two can never collide.
pub const PLACEHOLDER_PREFIX: &str = "local:";

/// Whether an identifier is still waiting for the server to name it.
pub fn is_placeholder(id: &str) -> bool {
    id.starts_with(PLACEHOLDER_PREFIX)
}

/// Mint an identifier for something the server has not seen yet.
pub fn new_placeholder() -> String {
    format!("{PLACEHOLDER_PREFIX}{}", Uuid::new_v4())
}

/// One replayable change.
///
/// `kind` is the serde tag *and* a column, so the diagnostics surface
/// can group a queue without parsing every payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mutation {
    SetFavorite {
        entity_type: String,
        entity_id: String,
        starred: bool,
    },
    /// `rating` is 1..=5; clearing is [`Mutation::ClearRating`] rather
    /// than a zero, so an accidental default can never read as "clear".
    SetRating {
        entity_type: String,
        entity_id: String,
        rating: u8,
    },
    ClearRating {
        entity_type: String,
        entity_id: String,
    },
    CreatePlaylist {
        /// The `local:` identifier the projection already shows.
        local_id: String,
        name: String,
        track_ids: Vec<String>,
    },
    /// A field left `None` is unchanged.
    ///
    /// Emptying one is a separate verb — see [`clear_comment`] — because
    /// the server coalesces an absent field and an explicit null
    /// identically. Naming the field is the only way to say "empty it",
    /// and it cannot fire by accident on a caller that simply omits it.
    ///
    /// [`clear_comment`]: Mutation::UpdatePlaylist::clear_comment
    UpdatePlaylist {
        playlist_id: String,
        name: Option<String>,
        comment: Option<String>,
        public: Option<bool>,
        /// Appended after the removals below, matching the order the
        /// server applies them in.
        add: Vec<String>,
        remove_indexes: Vec<usize>,
        /// Blank the comment out. `comment` is the only clearable field
        /// on a playlist.
        ///
        /// `default` on purpose: rows queued by a build that predates
        /// this field still deserialize, and they mean "clear nothing".
        #[serde(default)]
        clear_comment: bool,
    },
    DeletePlaylist {
        playlist_id: String,
    },
    Scrobble {
        track_id: String,
        submission: bool,
        played_at: i64,
    },
    SaveQueue {
        track_ids: Vec<String>,
        current: Option<String>,
        position_ms: i64,
        client: Option<String>,
    },
    /// A share created offline has neither an identifier nor a link
    /// until the server answers, so it carries a placeholder like a
    /// playlist does. Replay is safe without any special handling: the
    /// share token is derived deterministically from the identifier, so
    /// re-sending a creation yields the same URL rather than a second
    /// share.
    CreateShare {
        local_id: String,
        track_ids: Vec<String>,
        description: Option<String>,
        expires_at: Option<i64>,
    },
    /// Same shape as [`Mutation::UpdatePlaylist`], with two clearable
    /// fields instead of one.
    ///
    /// Clearing the expiry is the one that matters: without it, an
    /// expiry set by mistake would be permanent, and the owner's only
    /// recourse would be deleting the share and publishing a different
    /// URL.
    UpdateShare {
        share_id: String,
        description: Option<String>,
        expires_at: Option<i64>,
        #[serde(default)]
        clear_description: bool,
        #[serde(default)]
        clear_expires_at: bool,
    },
    DeleteShare {
        share_id: String,
    },
}

impl Mutation {
    /// Stable discriminant for the `kind` column.
    pub fn kind(&self) -> &'static str {
        match self {
            Mutation::SetFavorite { .. } => "set_favorite",
            Mutation::SetRating { .. } => "set_rating",
            Mutation::ClearRating { .. } => "clear_rating",
            Mutation::CreatePlaylist { .. } => "create_playlist",
            Mutation::UpdatePlaylist { .. } => "update_playlist",
            Mutation::DeletePlaylist { .. } => "delete_playlist",
            Mutation::Scrobble { .. } => "scrobble",
            Mutation::SaveQueue { .. } => "save_queue",
            Mutation::CreateShare { .. } => "create_share",
            Mutation::UpdateShare { .. } => "update_share",
            Mutation::DeleteShare { .. } => "delete_share",
        }
    }

    /// The placeholder this mutation depends on, if any. Used to find
    /// the entries a completed creation has to rewrite.
    pub fn placeholder_dependency(&self) -> Option<&str> {
        let id = match self {
            Mutation::CreatePlaylist { local_id, .. } => local_id,
            Mutation::UpdatePlaylist { playlist_id, .. } => playlist_id,
            Mutation::DeletePlaylist { playlist_id } => playlist_id,
            Mutation::CreateShare { local_id, .. } => local_id,
            Mutation::UpdateShare { share_id, .. } => share_id,
            Mutation::DeleteShare { share_id } => share_id,
            _ => return None,
        };
        is_placeholder(id).then_some(id.as_str())
    }
}

/// A queued entry as stored.
#[derive(Debug, Clone)]
pub struct QueuedMutation {
    pub id: i64,
    pub operation_id: String,
    pub mutation: Mutation,
    // Surfaced for diagnostics and asserted by the tests; the drain itself
    // doesn't branch on the try count.
    #[allow(dead_code)]
    pub attempt_count: i64,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Queue a change, returning the operation identifier it will be sent
/// under.
///
/// Call this inside the same transaction as the optimistic local write.
/// A projection updated without a queued mutation silently drops the
/// user's change; a mutation queued without the projection update makes
/// the UI lie until the round-trip completes.
pub async fn enqueue(conn: &mut SqliteConnection, mutation: &Mutation) -> AppResult<String> {
    let operation_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO remote_mutation (operation_id, kind, payload, created_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(&operation_id)
    .bind(mutation.kind())
    .bind(serde_json::to_string(mutation).map_err(|err| {
        crate::error::AppError::Other(format!("could not serialize a mutation: {err}"))
    })?)
    .bind(now_ms())
    .execute(&mut *conn)
    .await?;
    Ok(operation_id)
}

/// Pending entries, oldest first. Permanently-failed rows are left out —
/// they are for the diagnostics surface, not for the drain.
pub async fn pending(conn: &mut SqliteConnection, limit: i64) -> AppResult<Vec<QueuedMutation>> {
    let rows = sqlx::query(
        "SELECT id, operation_id, payload, attempt_count
           FROM remote_mutation
          WHERE failed_at IS NULL
          ORDER BY id
          LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;

    let mut queued = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let payload: String = row.try_get("payload")?;
        match serde_json::from_str::<Mutation>(&payload) {
            Ok(mutation) => queued.push(QueuedMutation {
                id,
                operation_id: row.try_get("operation_id")?,
                mutation,
                attempt_count: row.try_get("attempt_count")?,
            }),
            Err(error) => {
                // A payload this build cannot parse can never be sent by
                // it — most likely a row written by a newer build. It must
                // not be skipped: the drain relies on FIFO order, so
                // letting a later entry overtake it could send an edit
                // before the creation it depends on. Mark it permanently
                // failed (surfacing it in diagnostics and excluding it from
                // the next pass) and stop here, so nothing behind it slips
                // past within this batch. The next pass resumes past it.
                tracing::warn!(
                    %error,
                    mutation_id = id,
                    "a queued mutation cannot be read by this build; marking it failed and stopping the pass"
                );
                mark_failed(conn, id, &format!("unreadable payload: {error}")).await?;
                break;
            }
        }
    }
    Ok(queued)
}

/// Drop an entry the server has accepted.
pub async fn remove(conn: &mut SqliteConnection, id: i64) -> AppResult<()> {
    sqlx::query("DELETE FROM remote_mutation WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Record an attempt that failed but may still succeed later.
pub async fn record_attempt(conn: &mut SqliteConnection, id: i64, error: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE remote_mutation
            SET attempt_count = attempt_count + 1, last_attempt_at = ?, last_error = ?
          WHERE id = ?",
    )
    .bind(now_ms())
    .bind(error)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Mark an entry that can never succeed.
///
/// The server answers `422` both for a malformed payload and for an
/// operation identifier reused with a different fingerprint. Neither
/// changes on retry, so the row stops being attempted and is surfaced
/// instead of spinning forever.
pub async fn mark_failed(conn: &mut SqliteConnection, id: i64, error: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE remote_mutation
            SET failed_at = ?, attempt_count = attempt_count + 1,
                last_attempt_at = ?, last_error = ?
          WHERE id = ?",
    )
    .bind(now_ms())
    .bind(now_ms())
    .bind(error)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Give a created playlist its real identifier, everywhere it is named.
///
/// Three places, and missing any one of them breaks something specific:
/// the projection row itself, its ordered tracks, and any mutation still
/// queued behind the creation. That last rewrite is the only time a
/// queued payload is edited, and it is sound because FIFO order
/// guarantees those entries have never been presented to the server —
/// so no fingerprint exists for them to conflict with.
///
/// The row is **copied, re-pointed and dropped** rather than renamed in
/// place. A primary key cannot be updated out from under the rows that
/// reference it — renaming the playlist first orphans its tracks, and
/// re-pointing the tracks first has them name a parent that does not
/// exist yet. Either order violates the foreign key; going through a
/// second row never does.
pub async fn resolve_placeholder(
    conn: &mut SqliteConnection,
    placeholder: &str,
    remote_id: &str,
) -> AppResult<()> {
    // Idempotent: a `/changes` echo of our own creation can project the
    // real playlist under `remote_id` before the drain resolves the
    // placeholder. If it did, copying the placeholder over it collides on
    // the primary key and re-pointing its tracks collides on
    // `(playlist_remote_id, position)`. In that case the projection is
    // authoritative — just drop the placeholder side.
    let already_projected: i64 =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM remote_playlist WHERE remote_id = ?)")
            .bind(remote_id)
            .fetch_one(&mut *conn)
            .await?;

    if already_projected != 0 {
        sqlx::query("DELETE FROM remote_playlist_track WHERE playlist_remote_id = ?")
            .bind(placeholder)
            .execute(&mut *conn)
            .await?;
        sqlx::query("DELETE FROM remote_playlist WHERE remote_id = ?")
            .bind(placeholder)
            .execute(&mut *conn)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO remote_playlist
                (remote_id, name, comment, is_public, created_at, updated_at)
             SELECT ?, name, comment, is_public, created_at, updated_at
               FROM remote_playlist WHERE remote_id = ?",
        )
        .bind(remote_id)
        .bind(placeholder)
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "UPDATE remote_playlist_track SET playlist_remote_id = ? WHERE playlist_remote_id = ?",
        )
        .bind(remote_id)
        .bind(placeholder)
        .execute(&mut *conn)
        .await?;
        sqlx::query("DELETE FROM remote_playlist WHERE remote_id = ?")
            .bind(placeholder)
            .execute(&mut *conn)
            .await?;
    }

    rewrite_queued_references(conn, placeholder, remote_id).await
}

/// The share equivalent, plus the one thing only the creator ever learns.
///
/// The journal never carries a share's URL, and it cannot be derived
/// locally — the token is keyed on a server-side instance secret. So the
/// creation response is the only moment this device can capture the
/// link, and a share created on another device stays link-less here for
/// good. Storing it now is not an optimisation; it is the only chance.
pub async fn resolve_share_placeholder(
    conn: &mut SqliteConnection,
    placeholder: &str,
    remote_id: &str,
    url: Option<&str>,
) -> AppResult<()> {
    // Idempotent, like `resolve_placeholder`: a `/changes` echo may have
    // projected the real share already. If it did, still capture the URL —
    // the journal never carries it, so the creation response is the only
    // chance — then drop the placeholder side.
    let already_projected: i64 =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM remote_share WHERE remote_id = ?)")
            .bind(remote_id)
            .fetch_one(&mut *conn)
            .await?;

    if already_projected != 0 {
        sqlx::query("UPDATE remote_share SET url = COALESCE(?, url) WHERE remote_id = ?")
            .bind(url)
            .bind(remote_id)
            .execute(&mut *conn)
            .await?;
        sqlx::query("DELETE FROM remote_share_track WHERE share_remote_id = ?")
            .bind(placeholder)
            .execute(&mut *conn)
            .await?;
        sqlx::query("DELETE FROM remote_share WHERE remote_id = ?")
            .bind(placeholder)
            .execute(&mut *conn)
            .await?;
    } else {
        // Copied, re-pointed, dropped — for the same foreign-key reason as
        // the playlist above.
        sqlx::query(
            "INSERT INTO remote_share
                (remote_id, description, expires_at, visit_count, url, created_at, updated_at)
             SELECT ?, description, expires_at, visit_count, COALESCE(?, url), created_at, updated_at
               FROM remote_share WHERE remote_id = ?",
        )
        .bind(remote_id)
        .bind(url)
        .bind(placeholder)
        .execute(&mut *conn)
        .await?;
        sqlx::query("UPDATE remote_share_track SET share_remote_id = ? WHERE share_remote_id = ?")
            .bind(remote_id)
            .bind(placeholder)
            .execute(&mut *conn)
            .await?;
        sqlx::query("DELETE FROM remote_share WHERE remote_id = ?")
            .bind(placeholder)
            .execute(&mut *conn)
            .await?;
    }

    rewrite_queued_references(conn, placeholder, remote_id).await
}

/// Drop every queued entry that names a placeholder the server will
/// never be told about.
///
/// Used when the user deletes something they created offline before it
/// was ever sent. Sending the creation and then a delete would be two
/// pointless round-trips describing something that briefly existed for
/// nobody; dropping both is the same outcome with none of the noise.
///
/// Safe for the same reason [`resolve_placeholder`] is: FIFO guarantees
/// that entries behind an unlanded creation have never been presented,
/// so the server holds no fingerprint for them.
pub async fn discard_for_placeholder(
    conn: &mut SqliteConnection,
    placeholder: &str,
) -> AppResult<()> {
    let rows = sqlx::query("SELECT id, payload FROM remote_mutation WHERE failed_at IS NULL")
        .fetch_all(&mut *conn)
        .await?;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let payload: String = row.try_get("payload")?;
        let Ok(mutation) = serde_json::from_str::<Mutation>(&payload) else {
            continue;
        };
        if mutation.placeholder_dependency() == Some(placeholder) {
            remove(&mut *conn, id).await?;
        }
    }
    Ok(())
}

/// Point every still-queued entry at the identifier the server chose.
async fn rewrite_queued_references(
    conn: &mut SqliteConnection,
    placeholder: &str,
    remote_id: &str,
) -> AppResult<()> {
    let rows = sqlx::query(
        "SELECT id, payload FROM remote_mutation WHERE failed_at IS NULL ORDER BY id",
    )
    .fetch_all(&mut *conn)
    .await?;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let payload: String = row.try_get("payload")?;
        let Ok(mutation) = serde_json::from_str::<Mutation>(&payload) else {
            continue;
        };
        if mutation.placeholder_dependency() != Some(placeholder) {
            continue;
        }
        let rewritten = match mutation {
            Mutation::UpdatePlaylist {
                name,
                comment,
                public,
                add,
                remove_indexes,
                clear_comment,
                ..
            } => Mutation::UpdatePlaylist {
                playlist_id: remote_id.to_string(),
                name,
                comment,
                public,
                add,
                remove_indexes,
                clear_comment,
            },
            Mutation::DeletePlaylist { .. } => Mutation::DeletePlaylist {
                playlist_id: remote_id.to_string(),
            },
            Mutation::UpdateShare {
                description,
                expires_at,
                clear_description,
                clear_expires_at,
                ..
            } => Mutation::UpdateShare {
                share_id: remote_id.to_string(),
                description,
                expires_at,
                clear_description,
                clear_expires_at,
            },
            Mutation::DeleteShare { .. } => Mutation::DeleteShare {
                share_id: remote_id.to_string(),
            },
            // The creation itself is the row being resolved; it is
            // removed by the drain on success, so there is nothing to
            // rewrite.
            other => other,
        };
        sqlx::query("UPDATE remote_mutation SET payload = ? WHERE id = ?")
            .bind(serde_json::to_string(&rewritten).map_err(|err| {
                crate::error::AppError::Other(format!("could not serialize a mutation: {err}"))
            })?)
            .bind(id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        for migration in [
            include_str!(
                "../../../../migrations/profile/20260810120000_remote_source_projection.sql"
            ),
            include_str!("../../../../migrations/profile/20260810140000_remote_track_cache.sql"),
            include_str!("../../../../migrations/profile/20260813090000_remote_track_full_hash.sql"),
            include_str!("../../../../migrations/profile/20260816120000_remote_track_artist_id.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&pool).await.unwrap();
        }
        pool
    }

    fn favorite(starred: bool) -> Mutation {
        Mutation::SetFavorite {
            entity_type: "track".into(),
            entity_id: "t1".into(),
            starred,
        }
    }

    #[tokio::test]
    async fn every_enqueue_draws_its_own_operation_id() {
        // Two identical gestures are two mutations. Reusing one
        // identifier for both would make the server treat the second as
        // a replay of the first and drop it.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let first = enqueue(&mut conn, &favorite(true)).await.unwrap();
        let second = enqueue(&mut conn, &favorite(true)).await.unwrap();
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn pending_comes_back_in_the_order_it_was_queued() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        enqueue(&mut conn, &favorite(true)).await.unwrap();
        enqueue(&mut conn, &favorite(false)).await.unwrap();

        let queued = pending(&mut conn, 10).await.unwrap();
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].mutation, favorite(true));
        assert_eq!(queued[1].mutation, favorite(false));
    }

    #[tokio::test]
    async fn a_permanently_failed_entry_leaves_the_queue_without_blocking_it() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        enqueue(&mut conn, &favorite(true)).await.unwrap();
        enqueue(&mut conn, &favorite(false)).await.unwrap();

        let queued = pending(&mut conn, 10).await.unwrap();
        mark_failed(&mut conn, queued[0].id, "422 conflict")
            .await
            .unwrap();

        let still_pending = pending(&mut conn, 10).await.unwrap();
        assert_eq!(still_pending.len(), 1);
        assert_eq!(still_pending[0].mutation, favorite(false));

        // Still readable for diagnostics, just not for the drain.
        let failed: i64 =
            sqlx::query_scalar("SELECT count(*) FROM remote_mutation WHERE failed_at IS NOT NULL")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(failed, 1);
    }

    #[tokio::test]
    async fn a_transient_attempt_keeps_the_entry_and_counts_the_try() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        enqueue(&mut conn, &favorite(true)).await.unwrap();
        let queued = pending(&mut conn, 10).await.unwrap();

        record_attempt(&mut conn, queued[0].id, "503").await.unwrap();
        record_attempt(&mut conn, queued[0].id, "503").await.unwrap();

        let again = pending(&mut conn, 10).await.unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].attempt_count, 2);
    }

    #[tokio::test]
    async fn a_payload_this_build_cannot_read_stops_the_pass_and_is_marked_failed() {
        // A row written by a newer build can never be sent by this one.
        // FIFO is load-bearing, so it must not be skipped over — the pass
        // stops at it, it is marked permanently failed, and the next pass
        // resumes past it. That way the later entry is still sent, just one
        // pass later, and never *before* something it might depend on.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query(
            "INSERT INTO remote_mutation (operation_id, kind, payload, created_at)
             VALUES ('op-future', 'teleport', '{\"kind\":\"teleport\"}', 0)",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        enqueue(&mut conn, &favorite(true)).await.unwrap();

        // First pass stops at the unreadable row — nothing behind it slips
        // past — and marks it failed.
        let first = pending(&mut conn, 10).await.unwrap();
        assert!(first.is_empty(), "the pass must stop at the unreadable row");
        let failed: i64 =
            sqlx::query_scalar("SELECT count(*) FROM remote_mutation WHERE failed_at IS NOT NULL")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(failed, 1);

        // Next pass resumes past it and returns the readable entry.
        let second = pending(&mut conn, 10).await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].mutation, favorite(true));
    }

    #[tokio::test]
    async fn resolving_a_placeholder_the_projection_already_created_is_idempotent() {
        // A `/changes` echo can materialize the real playlist under its
        // server id before the drain resolves the placeholder. Resolving
        // must then not collide on the primary key or duplicate tracks —
        // it drops the placeholder side and keeps the projected row.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        let local_id = new_placeholder();
        // Placeholder projection (what the user saw offline).
        sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES (?, 'Offline mix')")
            .bind(&local_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO remote_playlist_track VALUES (?, 0, 't1')")
            .bind(&local_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        // The real row, already projected from the server's echo.
        sqlx::query(
            "INSERT INTO remote_playlist (remote_id, name) VALUES ('server-uuid', 'Offline mix')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query("INSERT INTO remote_playlist_track VALUES ('server-uuid', 0, 't1')")
            .execute(&mut *conn)
            .await
            .unwrap();

        resolve_placeholder(&mut conn, &local_id, "server-uuid")
            .await
            .expect("resolving over an already-projected row must not error");

        // Exactly one playlist row (the real one), one track, no placeholder.
        let playlists: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_playlist")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(playlists, 1);
        let placeholder_gone: i64 =
            sqlx::query_scalar("SELECT count(*) FROM remote_playlist WHERE remote_id = ?")
                .bind(&local_id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(placeholder_gone, 0);
        let tracks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM remote_playlist_track WHERE playlist_remote_id = 'server-uuid'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(tracks, 1, "the projected track must not be duplicated");
    }

    #[tokio::test]
    async fn resolving_a_share_the_projection_already_created_still_captures_the_url() {
        // The share URL is learned only from the creation response. If the
        // real share was already projected (without a URL — the journal
        // never carries one), resolving must still write the URL in.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        let local_id = new_placeholder();
        sqlx::query("INSERT INTO remote_share (remote_id, description) VALUES (?, 'My mix')")
            .bind(&local_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        // Real share already projected, URL still null.
        sqlx::query(
            "INSERT INTO remote_share (remote_id, description) VALUES ('share-uuid', 'My mix')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();

        resolve_share_placeholder(&mut conn, &local_id, "share-uuid", Some("https://x/y"))
            .await
            .expect("resolving over an already-projected share must not error");

        let url: Option<String> =
            sqlx::query_scalar("SELECT url FROM remote_share WHERE remote_id = 'share-uuid'")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(url.as_deref(), Some("https://x/y"));
        let placeholder_gone: i64 =
            sqlx::query_scalar("SELECT count(*) FROM remote_share WHERE remote_id = ?")
                .bind(&local_id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(placeholder_gone, 0);
    }

    #[tokio::test]
    async fn resolving_a_placeholder_rewrites_the_projection_and_the_queue() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        // Pinned: this test only proves anything with the constraint on,
        // and it caught a real ordering bug precisely because it was.
        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(fk, 1, "foreign keys are off; this test proves nothing");

        let local_id = new_placeholder();
        assert!(is_placeholder(&local_id));

        // The optimistic projection a user sees immediately.
        sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES (?, 'Offline mix')")
            .bind(&local_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO remote_playlist_track VALUES (?, 0, 't1')")
            .bind(&local_id)
            .execute(&mut *conn)
            .await
            .unwrap();

        enqueue(
            &mut conn,
            &Mutation::CreatePlaylist {
                local_id: local_id.clone(),
                name: "Offline mix".into(),
                track_ids: vec!["t1".into()],
            },
        )
        .await
        .unwrap();
        // An edit made before the creation ever reached the server.
        enqueue(
            &mut conn,
            &Mutation::UpdatePlaylist {
                playlist_id: local_id.clone(),
                name: Some("Renamed offline".into()),
                comment: None,
                public: None,
                add: vec![],
                remove_indexes: vec![],
                clear_comment: false,
            },
        )
        .await
        .unwrap();

        resolve_placeholder(&mut conn, &local_id, "server-uuid")
            .await
            .unwrap();

        let name: String =
            sqlx::query_scalar("SELECT name FROM remote_playlist WHERE remote_id = 'server-uuid'")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(name, "Offline mix");

        let tracks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM remote_playlist_track WHERE playlist_remote_id = 'server-uuid'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(tracks, 1, "the ordered tracks did not follow the rename");

        let queued = pending(&mut conn, 10).await.unwrap();
        match &queued[1].mutation {
            Mutation::UpdatePlaylist { playlist_id, .. } => {
                assert_eq!(
                    playlist_id, "server-uuid",
                    "the queued edit still names a playlist the server has never heard of"
                );
            }
            other => panic!("unexpected mutation: {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolving_a_share_captures_the_url_the_journal_never_carries() {
        // The token is keyed on a server-side secret, so the creation
        // response is the only moment this device can learn the link.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let local_id = new_placeholder();

        sqlx::query("INSERT INTO remote_share (remote_id) VALUES (?)")
            .bind(&local_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO remote_share_track VALUES (?, 0, 't1')")
            .bind(&local_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        enqueue(
            &mut conn,
            &Mutation::DeleteShare {
                share_id: local_id.clone(),
            },
        )
        .await
        .unwrap();

        resolve_share_placeholder(
            &mut conn,
            &local_id,
            "share-uuid",
            Some("https://music.example/share/abc"),
        )
        .await
        .unwrap();

        let url: Option<String> =
            sqlx::query_scalar("SELECT url FROM remote_share WHERE remote_id = 'share-uuid'")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(url.as_deref(), Some("https://music.example/share/abc"));

        let tracks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM remote_share_track WHERE share_remote_id = 'share-uuid'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(tracks, 1);

        let queued = pending(&mut conn, 10).await.unwrap();
        assert_eq!(
            queued[0].mutation,
            Mutation::DeleteShare {
                share_id: "share-uuid".into()
            }
        );
    }

    #[tokio::test]
    async fn resolving_leaves_unrelated_entries_alone() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mine = new_placeholder();
        let other = new_placeholder();

        enqueue(
            &mut conn,
            &Mutation::DeletePlaylist {
                playlist_id: other.clone(),
            },
        )
        .await
        .unwrap();

        resolve_placeholder(&mut conn, &mine, "server-uuid")
            .await
            .unwrap();

        let queued = pending(&mut conn, 10).await.unwrap();
        assert_eq!(
            queued[0].mutation,
            Mutation::DeletePlaylist {
                playlist_id: other
            }
        );
    }

    #[tokio::test]
    async fn setting_a_field_and_clearing_it_are_two_operations() {
        // The clear is part of the server's operation fingerprint, so
        // "set an expiry" and "remove it" cannot share a replay
        // identifier — presenting one id with the other's payload is
        // rejected as a conflict.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        let set = enqueue(
            &mut conn,
            &Mutation::UpdateShare {
                share_id: "s1".into(),
                description: None,
                expires_at: Some(9_999),
                clear_description: false,
                clear_expires_at: false,
            },
        )
        .await
        .unwrap();
        let cleared = enqueue(
            &mut conn,
            &Mutation::UpdateShare {
                share_id: "s1".into(),
                description: None,
                expires_at: None,
                clear_description: false,
                clear_expires_at: true,
            },
        )
        .await
        .unwrap();

        assert_ne!(set, cleared);
        assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_row_queued_before_clearing_existed_still_reads() {
        // Backwards compatibility with payloads written by an earlier
        // build: the absent flags mean "clear nothing".
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query(
            "INSERT INTO remote_mutation (operation_id, kind, payload, created_at)
             VALUES ('op-old', 'update_share', ?, 0)",
        )
        .bind(r#"{"kind":"update_share","share_id":"s1","description":"a","expires_at":null}"#)
        .execute(&mut *conn)
        .await
        .unwrap();

        let queued = pending(&mut conn, 10).await.unwrap();
        assert_eq!(
            queued[0].mutation,
            Mutation::UpdateShare {
                share_id: "s1".into(),
                description: Some("a".into()),
                expires_at: None,
                clear_description: false,
                clear_expires_at: false,
            }
        );
    }

    #[test]
    fn a_server_identifier_is_never_mistaken_for_a_placeholder() {
        assert!(!is_placeholder("31be1c98-3984-40ff-8352-73fc6e2c29b5"));
        assert!(is_placeholder(&new_placeholder()));
    }

    #[test]
    fn only_playlist_mutations_carry_a_placeholder_dependency() {
        assert!(favorite(true).placeholder_dependency().is_none());
        let id = new_placeholder();
        assert_eq!(
            Mutation::DeletePlaylist {
                playlist_id: id.clone()
            }
            .placeholder_dependency(),
            Some(id.as_str())
        );
        // A playlist the server already knows is not a dependency.
        assert!(Mutation::DeletePlaylist {
            playlist_id: "real-uuid".into()
        }
        .placeholder_dependency()
        .is_none());
    }

    #[test]
    fn the_stored_shape_round_trips() {
        for mutation in [
            favorite(true),
            Mutation::SetRating {
                entity_type: "album".into(),
                entity_id: "a1".into(),
                rating: 4,
            },
            Mutation::ClearRating {
                entity_type: "track".into(),
                entity_id: "t1".into(),
            },
            Mutation::Scrobble {
                track_id: "t1".into(),
                submission: true,
                played_at: 1_000,
            },
            Mutation::SaveQueue {
                track_ids: vec!["t1".into()],
                current: Some("t1".into()),
                position_ms: 4200,
                client: Some("WaveFlow".into()),
            },
        ] {
            let json = serde_json::to_string(&mutation).unwrap();
            assert_eq!(serde_json::from_str::<Mutation>(&json).unwrap(), mutation);
        }
    }
}
