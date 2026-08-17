//! Pushing the outbound queue to the server.
//!
//! Walks [`super::mutation`] entries in order and turns each into the
//! business call it stands for, stamped with its operation identifier so
//! a lost response can be re-sent without applying the change twice.
//!
//! ## Why the pass stops on a transient failure
//!
//! Order is load-bearing: an edit must never overtake the creation it
//! depends on. So the first entry that might still succeed ends the
//! pass, and everything behind it waits for the next one. A *permanent*
//! failure cannot succeed however long we wait, so that entry is marked
//! and the drain moves on — otherwise one bad request would deadlock
//! every later change behind it, forever.

use crate::{
    error::AppResult,
    offline,
    remote::{
        client::{FailureKind, RemoteClient, RemoteFailure},
        mutation::{self, Mutation, QueuedMutation},
    },
    state::AppState,
};

/// Entries attempted per pass. A bound rather than a policy: the loop
/// runs again as soon as it finishes, and a cap keeps one pass from
/// holding a profile lease across thousands of round-trips.
const BATCH: i64 = 100;

/// Names the `clear` array accepts, spelled once.
///
/// A name the server does not recognize is **rejected with 422** rather
/// than ignored — deliberately, so a client asking to clear `expiresAt`
/// hears that nothing happened instead of being told all is well. That
/// makes these three strings load-bearing, and worth having in exactly
/// one place rather than inline at each call site.
const FIELD_COMMENT: &str = "comment";
const FIELD_DESCRIPTION: &str = "description";
const FIELD_EXPIRES_AT: &str = "expires_at";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DrainReport {
    pub sent: usize,
    /// Entries that can never succeed and were marked as such.
    pub failed: usize,
    /// `true` when the pass stopped early on something retryable.
    pub stopped_early: bool,
}

/// What a creation's reply carries that we must keep.
///
/// For a playlist that is only the identifier. For a share it is the
/// identifier **and the URL**, which the journal never carries and which
/// cannot be derived locally — the creation response is the only moment
/// this device can ever learn it.
#[derive(Debug, serde::Deserialize)]
struct Created {
    id: String,
    #[serde(default)]
    url: Option<String>,
}

/// Kick a pass without waiting for it.
///
/// Edge-triggered: called after a local change commits. The change is
/// already durable in the projection and the queue, so the user has
/// nothing to wait for, and a failure here costs a delay rather than a
/// loss — the entry stays queued for the next pass.
pub fn spawn(handle: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        let state = handle.state::<AppState>();
        if let Err(error) = drain(state.inner()).await {
            tracing::debug!(%error, "could not push the queue after a local change");
        }
    });
}

/// Push what is queued. Does nothing, successfully, when there is
/// nothing to push or nowhere to push it.
pub async fn drain(state: &AppState) -> AppResult<DrainReport> {
    let mut report = DrainReport::default();
    if offline::is_offline() {
        return Ok(report);
    }
    // Capture the profile once and pin BOTH the client and the pool to it,
    // so the whole pass reads tokens and writes the queue for the same
    // profile even if a switch lands mid-drain.
    let profile_id = state.require_profile_id().await?;
    let Some(client) = RemoteClient::try_build_for(state, profile_id).await? else {
        return Ok(report);
    };
    // Resolve the leased pool once and keep the handle bound for the whole
    // pass — re-resolving per entry both churns the lease and risks a
    // profile swap landing mid-drain, applying one profile's queue against
    // another's data.
    let pool = state.require_profile_pool_for(Some(profile_id)).await?;

    let queued = {
        let mut conn = pool.acquire().await?;
        mutation::pending(&mut conn, BATCH).await?
    };

    for entry in queued {
        match send(&client, &entry).await {
            Ok(created) => {
                let mut tx = pool.begin().await?;
                // The identifier and the entry's removal commit
                // together. Resolving without removing would re-send a
                // creation the server has already accepted, under an
                // identifier whose fingerprint now names a different
                // playlist — the exact conflict the protocol rejects.
                match (&entry.mutation, &created) {
                    (Mutation::CreatePlaylist { local_id, .. }, Some(created)) => {
                        mutation::resolve_placeholder(&mut tx, local_id, &created.id).await?;
                    }
                    (Mutation::CreateShare { local_id, .. }, Some(created)) => {
                        mutation::resolve_share_placeholder(
                            &mut tx,
                            local_id,
                            &created.id,
                            created.url.as_deref(),
                        )
                        .await?;
                    }
                    _ => {}
                }
                mutation::remove(&mut tx, entry.id).await?;
                tx.commit().await?;
                report.sent += 1;
            }
            Err(failure) => {
                let mut conn = pool.acquire().await?;
                match failure.kind {
                    FailureKind::Permanent => {
                        tracing::warn!(
                            operation = %entry.operation_id,
                            kind = entry.mutation.kind(),
                            %failure,
                            "a queued change was refused and will not be retried"
                        );
                        mutation::mark_failed(&mut conn, entry.id, &failure.to_string()).await?;
                        report.failed += 1;
                    }
                    FailureKind::Transient | FailureKind::Unauthorized => {
                        mutation::record_attempt(&mut conn, entry.id, &failure.to_string()).await?;
                        report.stopped_early = true;
                        return Ok(report);
                    }
                }
            }
        }
    }

    Ok(report)
}

/// Issue one queued change.
///
/// Returns the created playlist when the mutation was a creation, since
/// that is the one case where the server's reply carries something the
/// caller must keep.
async fn send(
    client: &RemoteClient<'_>,
    entry: &QueuedMutation,
) -> Result<Option<Created>, RemoteFailure> {
    let op = &entry.operation_id;
    match &entry.mutation {
        Mutation::SetFavorite {
            entity_type,
            entity_id,
            starred,
        } => {
            let method = if *starred {
                reqwest::Method::PUT
            } else {
                reqwest::Method::DELETE
            };
            let path = format!("/api/v2/favorites/{entity_type}/{entity_id}");
            client.send_ok(client.mutate(method, &path, op)).await?;
            Ok(None)
        }

        Mutation::SetRating {
            entity_type,
            entity_id,
            rating,
        } => {
            let path = format!("/api/v2/ratings/{entity_type}/{entity_id}");
            client
                .send_ok(
                    client
                        .mutate(reqwest::Method::PUT, &path, op)
                        .json(&serde_json::json!({ "rating": rating })),
                )
                .await?;
            Ok(None)
        }

        Mutation::ClearRating {
            entity_type,
            entity_id,
        } => {
            // Clearing is a rating of zero on the same endpoint — the
            // server turns it into a delete event. There is no DELETE
            // route for a rating.
            let path = format!("/api/v2/ratings/{entity_type}/{entity_id}");
            client
                .send_ok(
                    client
                        .mutate(reqwest::Method::PUT, &path, op)
                        .json(&serde_json::json!({ "rating": 0 })),
                )
                .await?;
            Ok(None)
        }

        Mutation::CreatePlaylist {
            name, track_ids, ..
        } => {
            let created: Created = client
                .send_json(
                    client
                        .mutate(reqwest::Method::POST, "/api/v2/playlists", op)
                        .json(&serde_json::json!({ "name": name, "track_ids": track_ids })),
                )
                .await?;
            Ok(Some(created))
        }

        Mutation::UpdatePlaylist {
            playlist_id,
            name,
            comment,
            public,
            add,
            remove_indexes,
            clear_comment,
        } => {
            if mutation::is_placeholder(playlist_id) {
                // The creation this depends on never landed, so the
                // server has no such playlist and never will under this
                // identifier. Permanent, and reported as such rather
                // than retried against a URL that cannot exist.
                return Err(RemoteFailure {
                    kind: FailureKind::Permanent,
                    status: None,
                    code: None,
                    message: "the playlist this edit belongs to was never created on the server"
                        .into(),
                });
            }
            let path = format!("/api/v2/playlists/{playlist_id}");
            let mut body = serde_json::Map::new();
            // Only the keys that carry an intent. Sending `null` would
            // mean the same thing to this server — it coalesces — but
            // omitting them keeps the request honest about what changed.
            if let Some(name) = name {
                body.insert("name".into(), serde_json::json!(name));
            }
            if let Some(comment) = comment {
                body.insert("comment".into(), serde_json::json!(comment));
            }
            if let Some(public) = public {
                body.insert("public".into(), serde_json::json!(public));
            }
            if !add.is_empty() {
                body.insert("add".into(), serde_json::json!(add));
            }
            if !remove_indexes.is_empty() {
                body.insert("remove_indexes".into(), serde_json::json!(remove_indexes));
            }
            if *clear_comment {
                body.insert("clear".into(), serde_json::json!([FIELD_COMMENT]));
            }
            client
                .send_ok(
                    client
                        .mutate(reqwest::Method::PATCH, &path, op)
                        .json(&serde_json::Value::Object(body)),
                )
                .await?;
            Ok(None)
        }

        Mutation::DeletePlaylist { playlist_id } => {
            if mutation::is_placeholder(playlist_id) {
                // Deleting something the server never received is
                // already true. Nothing to send, and nothing to retry.
                return Ok(None);
            }
            let path = format!("/api/v2/playlists/{playlist_id}");
            client
                .send_ok(client.mutate(reqwest::Method::DELETE, &path, op))
                .await?;
            Ok(None)
        }

        Mutation::Scrobble {
            track_id,
            submission,
            played_at,
        } => {
            client
                .send_ok(
                    client
                        .mutate(reqwest::Method::POST, "/api/v2/scrobbles", op)
                        .json(&serde_json::json!({
                            "track_id": track_id,
                            "submission": submission,
                            "played_at": played_at,
                        })),
                )
                .await?;
            Ok(None)
        }

        Mutation::SaveQueue {
            track_ids,
            current,
            position_ms,
            client: client_name,
        } => {
            client
                .send_ok(
                    client
                        .mutate(reqwest::Method::PUT, "/api/v2/queue", op)
                        .json(&serde_json::json!({
                            "track_ids": track_ids,
                            "current": current,
                            "position_ms": position_ms,
                            "client": client_name,
                        })),
                )
                .await?;
            Ok(None)
        }

        Mutation::CreateShare {
            track_ids,
            description,
            expires_at,
            ..
        } => {
            // No special handling for a replay: the share token is
            // derived deterministically from the identifier, so
            // re-sending returns the same URL rather than minting a
            // second share.
            let created: Created = client
                .send_json(
                    client
                        .mutate(reqwest::Method::POST, "/api/v2/shares", op)
                        .json(&serde_json::json!({
                            "track_ids": track_ids,
                            "description": description,
                            "expires_at": expires_at,
                        })),
                )
                .await?;
            Ok(Some(created))
        }

        Mutation::UpdateShare {
            share_id,
            description,
            expires_at,
            clear_description,
            clear_expires_at,
        } => {
            if mutation::is_placeholder(share_id) {
                return Err(RemoteFailure {
                    kind: FailureKind::Permanent,
                    status: None,
                    code: None,
                    message: "the share this edit belongs to was never created on the server"
                        .into(),
                });
            }
            let path = format!("/api/v2/shares/{share_id}");
            let mut body = serde_json::Map::new();
            if let Some(description) = description {
                body.insert("description".into(), serde_json::json!(description));
            }
            if let Some(expires_at) = expires_at {
                body.insert("expires_at".into(), serde_json::json!(expires_at));
            }
            let mut clear = Vec::new();
            if *clear_description {
                clear.push(FIELD_DESCRIPTION);
            }
            if *clear_expires_at {
                clear.push(FIELD_EXPIRES_AT);
            }
            if !clear.is_empty() {
                body.insert("clear".into(), serde_json::json!(clear));
            }
            client
                .send_ok(
                    client
                        .mutate(reqwest::Method::PATCH, &path, op)
                        .json(&serde_json::Value::Object(body)),
                )
                .await?;
            Ok(None)
        }

        Mutation::DeleteShare { share_id } => {
            if mutation::is_placeholder(share_id) {
                // Never reached the server, so it is already gone.
                return Ok(None);
            }
            let path = format!("/api/v2/shares/{share_id}");
            client
                .send_ok(client.mutate(reqwest::Method::DELETE, &path, op))
                .await?;
            Ok(None)
        }
    }
}
