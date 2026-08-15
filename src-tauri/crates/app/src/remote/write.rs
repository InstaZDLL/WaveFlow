//! Local gestures on remote data: write here, then send.
//!
//! Every gesture below does the same two things in **one transaction**,
//! and that pairing is the whole point of the module:
//!
//! 1. apply the change to the projection, so the interface reflects it
//!    immediately, offline included;
//! 2. queue the matching mutation, so the server hears about it.
//!
//! Splitting them is the failure this module exists to prevent. A
//! projection updated without a queued mutation silently discards the
//! user's change the next time a snapshot lands. A mutation queued
//! without the projection update makes the interface lie until the
//! round-trip completes — and lie *backwards*, showing the old value
//! while the new one is already on its way.
//!
//! Each gesture is split in two: a `*_in_tx` half that does the real
//! work against a connection, and a public wrapper that owns the
//! transaction and the profile lease. The split is not ceremony — it is
//! what lets the pairing be tested, since an `AppState` cannot be built
//! in a unit test.
//!
//! ## The drain is kicked by the caller, not waited on
//!
//! These functions commit and return; the command layer then wakes the
//! drain through [`super::drain::spawn`] and answers immediately. The
//! change is already durable, so the round-trip is not something the
//! user should be made to watch. Offline, the wake is a no-op and the
//! entry simply waits.
//!
//! ## What comes back through `/changes` is not a problem
//!
//! The server echoes every one of these as a journal event, and applying
//! our own change a second time is harmless: every write in
//! [`super::projection`] is an upsert or a delete keyed by identity, so
//! the second application is the same as the first.

use sqlx::SqliteConnection;

use crate::{
    error::{AppError, AppResult},
    remote::mutation::{self, Mutation},
    state::AppState,
};

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Star or unstar a remote track, album or artist.
pub async fn set_favorite(
    state: &AppState,
    entity_type: &str,
    entity_id: &str,
    starred: bool,
) -> AppResult<()> {
    let (pool, profile_id) = lease(state).await?;
    let _ = profile_id;
    let mut tx = pool.begin().await?;
    set_favorite_in_tx(&mut tx, entity_type, entity_id, starred).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_favorite_in_tx(
    conn: &mut SqliteConnection,
    entity_type: &str,
    entity_id: &str,
    starred: bool,
) -> AppResult<()> {
    if starred {
        sqlx::query(
            "INSERT INTO remote_favorite (entity_type, entity_id, starred_at)
             VALUES (?, ?, ?)
             ON CONFLICT(entity_type, entity_id) DO UPDATE
                SET starred_at = excluded.starred_at",
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(now_ms())
        .execute(&mut *conn)
        .await?;
    } else {
        sqlx::query("DELETE FROM remote_favorite WHERE entity_type = ? AND entity_id = ?")
            .bind(entity_type)
            .bind(entity_id)
            .execute(&mut *conn)
            .await?;
    }
    mutation::enqueue(
        conn,
        &Mutation::SetFavorite {
            entity_type: entity_type.to_string(),
            entity_id: entity_id.to_string(),
            starred,
        },
    )
    .await?;
    Ok(())
}

/// Rate a remote entity, or clear its rating with `0`.
pub async fn set_rating(
    state: &AppState,
    entity_type: &str,
    entity_id: &str,
    rating: u8,
) -> AppResult<()> {
    let (pool, _) = lease(state).await?;
    let mut tx = pool.begin().await?;
    set_rating_in_tx(&mut tx, entity_type, entity_id, rating).await?;
    tx.commit().await?;
    Ok(())
}

/// `0` clears. It becomes a distinct mutation rather than a stored
/// value, because a stored zero would mean "rated nothing", which is not
/// a rating.
///
/// The range is checked here rather than in the wrapper so every caller
/// inherits it — and so it can be tested, which the wrapper cannot be.
pub async fn set_rating_in_tx(
    conn: &mut SqliteConnection,
    entity_type: &str,
    entity_id: &str,
    rating: u8,
) -> AppResult<()> {
    if rating > 5 {
        return Err(AppError::Other("a rating is 0 to 5 stars".into()));
    }
    let mutation = if rating == 0 {
        sqlx::query("DELETE FROM remote_rating WHERE entity_type = ? AND entity_id = ?")
            .bind(entity_type)
            .bind(entity_id)
            .execute(&mut *conn)
            .await?;
        Mutation::ClearRating {
            entity_type: entity_type.to_string(),
            entity_id: entity_id.to_string(),
        }
    } else {
        sqlx::query(
            "INSERT INTO remote_rating (entity_type, entity_id, rating, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(entity_type, entity_id) DO UPDATE
                SET rating = excluded.rating, updated_at = excluded.updated_at",
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(i64::from(rating))
        .bind(now_ms())
        .execute(&mut *conn)
        .await?;
        Mutation::SetRating {
            entity_type: entity_type.to_string(),
            entity_id: entity_id.to_string(),
            rating,
        }
    };
    mutation::enqueue(conn, &mutation).await?;
    Ok(())
}

/// Create a playlist the server has never heard of.
///
/// It gets a `local:` identifier so the projection can key on it like
/// any other, which is what makes it visible and editable straight away.
/// Returns that identifier — a caller holding it should re-read rather
/// than assume it survives, since the server's replaces it.
pub async fn create_playlist(
    state: &AppState,
    name: &str,
    track_ids: &[String],
) -> AppResult<String> {
    let (pool, _) = lease(state).await?;
    let mut tx = pool.begin().await?;
    let id = create_playlist_in_tx(&mut tx, name, track_ids).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn create_playlist_in_tx(
    conn: &mut SqliteConnection,
    name: &str,
    track_ids: &[String],
) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Other("a playlist needs a name".into()));
    }
    let local_id = mutation::new_placeholder();
    let now = now_ms();

    sqlx::query(
        "INSERT INTO remote_playlist (remote_id, name, is_public, created_at, updated_at)
         VALUES (?, ?, 0, ?, ?)",
    )
    .bind(&local_id)
    .bind(name)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;

    for (position, track_id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO remote_playlist_track (playlist_remote_id, position, track_remote_id)
             VALUES (?, ?, ?)",
        )
        .bind(&local_id)
        .bind(position as i64)
        .bind(track_id)
        .execute(&mut *conn)
        .await?;
    }

    mutation::enqueue(
        conn,
        &Mutation::CreatePlaylist {
            local_id: local_id.clone(),
            name: name.to_string(),
            track_ids: track_ids.to_vec(),
        },
    )
    .await?;
    Ok(local_id)
}

/// Rename a playlist, set or empty its comment, change its visibility.
pub async fn update_playlist(
    state: &AppState,
    playlist_id: &str,
    name: Option<String>,
    comment: Option<String>,
    public: Option<bool>,
    clear_comment: bool,
) -> AppResult<()> {
    let (pool, _) = lease(state).await?;
    let mut tx = pool.begin().await?;
    update_playlist_in_tx(&mut tx, playlist_id, name, comment, public, clear_comment).await?;
    tx.commit().await?;
    Ok(())
}

/// `clear_comment` empties the comment, which omitting it cannot do —
/// the server coalesces an absent field onto the current value, so
/// naming the field is the only way to say "empty".
pub async fn update_playlist_in_tx(
    conn: &mut SqliteConnection,
    playlist_id: &str,
    name: Option<String>,
    comment: Option<String>,
    public: Option<bool>,
    clear_comment: bool,
) -> AppResult<()> {
    if let Some(name) = &name {
        sqlx::query("UPDATE remote_playlist SET name = ? WHERE remote_id = ?")
            .bind(name)
            .bind(playlist_id)
            .execute(&mut *conn)
            .await?;
    }
    // The clear wins over a value: asking for both is contradictory, and
    // emptying is the more explicit of the two intents.
    if clear_comment {
        sqlx::query("UPDATE remote_playlist SET comment = NULL WHERE remote_id = ?")
            .bind(playlist_id)
            .execute(&mut *conn)
            .await?;
    } else if let Some(comment) = &comment {
        sqlx::query("UPDATE remote_playlist SET comment = ? WHERE remote_id = ?")
            .bind(comment)
            .bind(playlist_id)
            .execute(&mut *conn)
            .await?;
    }
    if let Some(public) = public {
        sqlx::query("UPDATE remote_playlist SET is_public = ? WHERE remote_id = ?")
            .bind(i64::from(public))
            .bind(playlist_id)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query("UPDATE remote_playlist SET updated_at = ? WHERE remote_id = ?")
        .bind(now_ms())
        .bind(playlist_id)
        .execute(&mut *conn)
        .await?;

    mutation::enqueue(
        conn,
        &Mutation::UpdatePlaylist {
            playlist_id: playlist_id.to_string(),
            name,
            comment: if clear_comment { None } else { comment },
            public,
            add: Vec::new(),
            remove_indexes: Vec::new(),
            clear_comment,
        },
    )
    .await?;
    Ok(())
}

/// Delete a playlist.
pub async fn delete_playlist(state: &AppState, playlist_id: &str) -> AppResult<()> {
    let (pool, _) = lease(state).await?;
    let mut tx = pool.begin().await?;
    delete_playlist_in_tx(&mut tx, playlist_id).await?;
    tx.commit().await?;
    Ok(())
}

/// A playlist still carrying a placeholder never reached the server, so
/// its queued creation is **dropped** rather than sent and immediately
/// undone. Two round-trips describing something that briefly existed for
/// nobody is the same outcome with more noise.
pub async fn delete_playlist_in_tx(
    conn: &mut SqliteConnection,
    playlist_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM remote_playlist_track WHERE playlist_remote_id = ?")
        .bind(playlist_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM remote_playlist WHERE remote_id = ?")
        .bind(playlist_id)
        .execute(&mut *conn)
        .await?;

    if mutation::is_placeholder(playlist_id) {
        mutation::discard_for_placeholder(&mut *conn, playlist_id).await?;
        return Ok(());
    }

    mutation::enqueue(
        conn,
        &Mutation::DeletePlaylist {
            playlist_id: playlist_id.to_string(),
        },
    )
    .await?;
    Ok(())
}

/// Record a play against the remote account.
///
/// `submission: false` is a "now playing" ping, `true` a completed
/// listen.
///
/// ## Not hooked to the player, and why
///
/// Nothing calls this from playback yet, and wiring it there today
/// would be a bug rather than a feature: the local queue holds file
/// paths and local row ids, and the scrobbler joins the local `track`
/// table. Those identifiers mean nothing to the server, which validates
/// them — every such mutation would come back `404`, be marked
/// permanently failed, and pile up in the queue as garbage a user would
/// have to be told about.
///
/// The dependency is remote playback, which does not exist. Until it
/// does, this is the surface a caller holding a genuine remote track
/// identifier uses.
pub async fn scrobble(
    state: &AppState,
    track_id: &str,
    submission: bool,
    played_at: Option<i64>,
) -> AppResult<()> {
    let (pool, _) = lease(state).await?;
    let mut tx = pool.begin().await?;
    scrobble_in_tx(&mut tx, track_id, submission, played_at).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn scrobble_in_tx(
    conn: &mut SqliteConnection,
    track_id: &str,
    submission: bool,
    played_at: Option<i64>,
) -> AppResult<()> {
    let played_at = played_at.unwrap_or_else(now_ms);
    // Only a completed listen belongs in the history. A "now playing"
    // ping is a transient state the server tracks separately, and
    // writing it here would show a play that never happened.
    if submission {
        sqlx::query(
            "INSERT OR REPLACE INTO remote_history (track_remote_id, played_at, submission)
             VALUES (?, ?, 1)",
        )
        .bind(track_id)
        .bind(played_at)
        .execute(&mut *conn)
        .await?;
    }
    mutation::enqueue(
        conn,
        &Mutation::Scrobble {
            track_id: track_id.to_string(),
            submission,
            played_at,
        },
    )
    .await?;
    Ok(())
}

/// Save the account's play queue.
///
/// Same dependency as [`scrobble`]: the identifiers have to be the
/// server's, so nothing drives this from the local player yet.
pub async fn save_queue(
    state: &AppState,
    track_ids: &[String],
    current: Option<String>,
    position_ms: i64,
    client: Option<String>,
) -> AppResult<()> {
    let (pool, _) = lease(state).await?;
    let mut tx = pool.begin().await?;
    save_queue_in_tx(&mut tx, track_ids, current, position_ms, client).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn save_queue_in_tx(
    conn: &mut SqliteConnection,
    track_ids: &[String],
    current: Option<String>,
    position_ms: i64,
    client: Option<String>,
) -> AppResult<()> {
    let position_ms = position_ms.max(0);
    sqlx::query(
        "INSERT INTO remote_queue (id, current_remote_id, position_ms, changed_by, updated_at)
         VALUES (1, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
            current_remote_id = excluded.current_remote_id,
            position_ms       = excluded.position_ms,
            changed_by        = excluded.changed_by,
            updated_at        = excluded.updated_at",
    )
    .bind(current.as_deref())
    .bind(position_ms)
    .bind(client.as_deref())
    .bind(now_ms())
    .execute(&mut *conn)
    .await?;

    sqlx::query("DELETE FROM remote_queue_track")
        .execute(&mut *conn)
        .await?;
    for (position, track_id) in track_ids.iter().enumerate() {
        sqlx::query("INSERT INTO remote_queue_track (position, track_remote_id) VALUES (?, ?)")
            .bind(position as i64)
            .bind(track_id)
            .execute(&mut *conn)
            .await?;
    }

    mutation::enqueue(
        conn,
        &Mutation::SaveQueue {
            track_ids: track_ids.to_vec(),
            current,
            position_ms,
            client,
        },
    )
    .await?;
    Ok(())
}

/// Publish a share of remote tracks.
///
/// Returns the local identifier. The public link is **not** known yet —
/// it only comes back with the server's response, because the token is
/// derived from a server-side secret. A share created offline therefore
/// has no link until it lands.
pub async fn create_share(
    state: &AppState,
    track_ids: &[String],
    description: Option<String>,
    expires_at: Option<i64>,
) -> AppResult<String> {
    let (pool, _) = lease(state).await?;
    let mut tx = pool.begin().await?;
    let id = create_share_in_tx(&mut tx, track_ids, description, expires_at).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn create_share_in_tx(
    conn: &mut SqliteConnection,
    track_ids: &[String],
    description: Option<String>,
    expires_at: Option<i64>,
) -> AppResult<String> {
    if track_ids.is_empty() {
        return Err(AppError::Other("a share needs at least one track".into()));
    }
    let local_id = mutation::new_placeholder();
    let now = now_ms();

    sqlx::query(
        "INSERT INTO remote_share (remote_id, description, expires_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&local_id)
    .bind(description.as_deref())
    .bind(expires_at)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    for (position, track_id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO remote_share_track (share_remote_id, position, track_remote_id)
             VALUES (?, ?, ?)",
        )
        .bind(&local_id)
        .bind(position as i64)
        .bind(track_id)
        .execute(&mut *conn)
        .await?;
    }

    mutation::enqueue(
        conn,
        &Mutation::CreateShare {
            local_id: local_id.clone(),
            track_ids: track_ids.to_vec(),
            description,
            expires_at,
        },
    )
    .await?;
    Ok(local_id)
}

/// Change a share's description or expiry, or empty either.
pub async fn update_share(
    state: &AppState,
    share_id: &str,
    description: Option<String>,
    expires_at: Option<i64>,
    clear_description: bool,
    clear_expires_at: bool,
) -> AppResult<()> {
    let (pool, _) = lease(state).await?;
    let mut tx = pool.begin().await?;
    update_share_in_tx(
        &mut tx,
        share_id,
        description,
        expires_at,
        clear_description,
        clear_expires_at,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The clears are what make an expiry removable at all — omitting the
/// field leaves it in place, so a share given one by mistake would
/// otherwise be stuck with it for good.
pub async fn update_share_in_tx(
    conn: &mut SqliteConnection,
    share_id: &str,
    description: Option<String>,
    expires_at: Option<i64>,
    clear_description: bool,
    clear_expires_at: bool,
) -> AppResult<()> {
    if clear_description {
        sqlx::query("UPDATE remote_share SET description = NULL WHERE remote_id = ?")
            .bind(share_id)
            .execute(&mut *conn)
            .await?;
    } else if let Some(description) = &description {
        sqlx::query("UPDATE remote_share SET description = ? WHERE remote_id = ?")
            .bind(description)
            .bind(share_id)
            .execute(&mut *conn)
            .await?;
    }
    if clear_expires_at {
        sqlx::query("UPDATE remote_share SET expires_at = NULL WHERE remote_id = ?")
            .bind(share_id)
            .execute(&mut *conn)
            .await?;
    } else if let Some(expires_at) = expires_at {
        sqlx::query("UPDATE remote_share SET expires_at = ? WHERE remote_id = ?")
            .bind(expires_at)
            .bind(share_id)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query("UPDATE remote_share SET updated_at = ? WHERE remote_id = ?")
        .bind(now_ms())
        .bind(share_id)
        .execute(&mut *conn)
        .await?;

    mutation::enqueue(
        conn,
        &Mutation::UpdateShare {
            share_id: share_id.to_string(),
            description: if clear_description { None } else { description },
            expires_at: if clear_expires_at { None } else { expires_at },
            clear_description,
            clear_expires_at,
        },
    )
    .await?;
    Ok(())
}

/// Withdraw a share. Same placeholder handling as a playlist: one that
/// never reached the server takes its queued creation with it.
pub async fn delete_share(state: &AppState, share_id: &str) -> AppResult<()> {
    let (pool, _) = lease(state).await?;
    let mut tx = pool.begin().await?;
    delete_share_in_tx(&mut tx, share_id).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn delete_share_in_tx(
    conn: &mut SqliteConnection,
    share_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM remote_share_track WHERE share_remote_id = ?")
        .bind(share_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM remote_share WHERE remote_id = ?")
        .bind(share_id)
        .execute(&mut *conn)
        .await?;

    if mutation::is_placeholder(share_id) {
        mutation::discard_for_placeholder(&mut *conn, share_id).await?;
        return Ok(());
    }

    mutation::enqueue(
        conn,
        &Mutation::DeleteShare {
            share_id: share_id.to_string(),
        },
    )
    .await?;
    Ok(())
}

/// The active profile's pool, pinned to the profile it was resolved
/// from, so a switch landing mid-write fails the call instead of
/// applying one profile's gesture to another's data.
async fn lease(state: &AppState) -> AppResult<(crate::state::ProfilePool, i64)> {
    let profile_id = state.require_profile_id().await?;
    let pool = state.require_profile_pool_for(Some(profile_id)).await?;
    Ok((pool, profile_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::mutation::pending;
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
        ] {
            sqlx::raw_sql(migration).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn count(conn: &mut SqliteConnection, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
            .fetch_one(&mut *conn)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn starring_writes_the_projection_and_queues_the_mutation() {
        // The pairing is the module's reason to exist: one without the
        // other either loses the change or shows the wrong value.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        set_favorite_in_tx(&mut conn, "track", "t1", true)
            .await
            .unwrap();

        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_favorite").await, 1);
        let queued = pending(&mut conn, 10).await.unwrap();
        assert_eq!(
            queued[0].mutation,
            Mutation::SetFavorite {
                entity_type: "track".into(),
                entity_id: "t1".into(),
                starred: true
            }
        );
    }

    #[tokio::test]
    async fn unstarring_removes_the_row_and_still_queues() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        set_favorite_in_tx(&mut conn, "track", "t1", true).await.unwrap();
        set_favorite_in_tx(&mut conn, "track", "t1", false).await.unwrap();

        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_favorite").await, 0);
        assert_eq!(
            pending(&mut conn, 10).await.unwrap().len(),
            2,
            "the removal has to travel too"
        );
    }

    #[tokio::test]
    async fn a_zero_rating_clears_locally_and_queues_a_clear() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        set_rating_in_tx(&mut conn, "track", "t1", 4).await.unwrap();
        set_rating_in_tx(&mut conn, "track", "t1", 0).await.unwrap();

        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_rating").await, 0);
        let queued = pending(&mut conn, 10).await.unwrap();
        assert_eq!(
            queued[1].mutation,
            Mutation::ClearRating {
                entity_type: "track".into(),
                entity_id: "t1".into()
            },
            "a zero must not travel as a rating of zero"
        );
    }

    #[tokio::test]
    async fn a_rating_above_five_is_refused_before_anything_is_written() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        assert!(set_rating_in_tx(&mut conn, "track", "t1", 6).await.is_err());

        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_rating").await, 0);
        assert_eq!(
            count(&mut conn, "SELECT count(*) FROM remote_mutation").await,
            0,
            "a refused gesture must not leave a queued mutation behind"
        );
    }

    #[tokio::test]
    async fn a_new_playlist_is_visible_immediately_and_queued() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        let id = create_playlist_in_tx(&mut conn, "  Offline mix  ", &["t1".into(), "t2".into()])
            .await
            .unwrap();

        assert!(mutation::is_placeholder(&id));
        let name: String =
            sqlx::query_scalar("SELECT name FROM remote_playlist WHERE remote_id = ?")
                .bind(&id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(name, "Offline mix", "the name was not trimmed");
        assert_eq!(
            count(&mut conn, "SELECT count(*) FROM remote_playlist_track").await,
            2
        );
        assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_nameless_playlist_is_refused() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        assert!(create_playlist_in_tx(&mut conn, "   ", &[]).await.is_err());
    }

    #[tokio::test]
    async fn clearing_a_comment_wins_over_setting_one() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let id = create_playlist_in_tx(&mut conn, "Mix", &[]).await.unwrap();
        update_playlist_in_tx(&mut conn, &id, None, Some("notes".into()), None, false)
            .await
            .unwrap();

        // Contradictory input: a value and a clear. The clear is the
        // more explicit intent and must not silently lose.
        update_playlist_in_tx(&mut conn, &id, None, Some("ignored".into()), None, true)
            .await
            .unwrap();

        let comment: Option<String> =
            sqlx::query_scalar("SELECT comment FROM remote_playlist WHERE remote_id = ?")
                .bind(&id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(comment, None);

        let queued = pending(&mut conn, 10).await.unwrap();
        match &queued.last().unwrap().mutation {
            Mutation::UpdatePlaylist {
                comment,
                clear_comment,
                ..
            } => {
                assert!(clear_comment);
                assert_eq!(
                    comment, &None,
                    "sending both would let the server coalesce the value back in"
                );
            }
            other => panic!("unexpected mutation: {other:?}"),
        }
    }

    #[tokio::test]
    async fn deleting_an_unsent_playlist_drops_its_queued_creation() {
        // Sending a creation and then a delete describes something that
        // existed for nobody. Both are dropped instead.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let id = create_playlist_in_tx(&mut conn, "Offline", &["t1".into()])
            .await
            .unwrap();
        assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 1);

        delete_playlist_in_tx(&mut conn, &id).await.unwrap();

        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_playlist").await, 0);
        assert_eq!(
            count(&mut conn, "SELECT count(*) FROM remote_playlist_track").await,
            0
        );
        assert!(
            pending(&mut conn, 10).await.unwrap().is_empty(),
            "the creation should not travel for something already deleted"
        );
    }

    #[tokio::test]
    async fn deleting_a_playlist_the_server_knows_queues_the_delete() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES ('server-id', 'Mix')")
            .execute(&mut *conn)
            .await
            .unwrap();

        delete_playlist_in_tx(&mut conn, "server-id").await.unwrap();

        let queued = pending(&mut conn, 10).await.unwrap();
        assert_eq!(
            queued[0].mutation,
            Mutation::DeletePlaylist {
                playlist_id: "server-id".into()
            }
        );
    }

    #[tokio::test]
    async fn a_failed_write_leaves_neither_half_behind() {
        // The transaction is what guarantees this. Rolling back after a
        // projection write must take the queued mutation with it.
        let pool = pool().await;
        let mut tx = pool.begin().await.unwrap();
        set_favorite_in_tx(&mut tx, "track", "t1", true).await.unwrap();
        tx.rollback().await.unwrap();

        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_favorite").await, 0);
        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_mutation").await, 0);
    }

    #[tokio::test]
    async fn a_now_playing_ping_travels_without_entering_the_history() {
        // It is a transient state, not a play. Writing it locally would
        // show a listen that never happened.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        scrobble_in_tx(&mut conn, "t1", false, Some(1_000)).await.unwrap();
        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_history").await, 0);
        assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 1);

        scrobble_in_tx(&mut conn, "t1", true, Some(2_000)).await.unwrap();
        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_history").await, 1);
    }

    #[tokio::test]
    async fn saving_the_queue_replaces_it_rather_than_appending() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        save_queue_in_tx(&mut conn, &["a".into(), "b".into()], Some("a".into()), 0, None)
            .await
            .unwrap();
        save_queue_in_tx(&mut conn, &["c".into()], Some("c".into()), 4200, None)
            .await
            .unwrap();

        let ids: Vec<String> =
            sqlx::query_scalar("SELECT track_remote_id FROM remote_queue_track ORDER BY position")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        assert_eq!(ids, vec!["c"]);
        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_queue").await, 1);
    }

    #[tokio::test]
    async fn a_negative_position_never_reaches_storage() {
        // The column has a CHECK, so a stray negative would abort the
        // whole gesture rather than degrade.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        save_queue_in_tx(&mut conn, &["a".into()], None, -5, None)
            .await
            .unwrap();
        let position: i64 = sqlx::query_scalar("SELECT position_ms FROM remote_queue")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(position, 0);
    }

    #[tokio::test]
    async fn a_share_is_visible_at_once_but_has_no_link_yet() {
        // The token is derived from a server-side secret, so the link
        // cannot exist before the creation lands.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        let id = create_share_in_tx(&mut conn, &["t1".into()], Some("notes".into()), None)
            .await
            .unwrap();

        assert!(mutation::is_placeholder(&id));
        let url: Option<String> =
            sqlx::query_scalar("SELECT url FROM remote_share WHERE remote_id = ?")
                .bind(&id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(url, None);
        assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_empty_share_is_refused() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        assert!(create_share_in_tx(&mut conn, &[], None, None).await.is_err());
    }

    #[tokio::test]
    async fn clearing_an_expiry_empties_it_locally_and_says_so_upstream() {
        // The case that motivated the whole clear verb: without it an
        // expiry set by mistake is permanent.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let id = create_share_in_tx(&mut conn, &["t1".into()], None, Some(9_999))
            .await
            .unwrap();

        update_share_in_tx(&mut conn, &id, None, None, false, true)
            .await
            .unwrap();

        let expires: Option<i64> =
            sqlx::query_scalar("SELECT expires_at FROM remote_share WHERE remote_id = ?")
                .bind(&id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(expires, None);

        match &pending(&mut conn, 10).await.unwrap().last().unwrap().mutation {
            Mutation::UpdateShare {
                clear_expires_at,
                expires_at,
                ..
            } => {
                assert!(clear_expires_at);
                assert_eq!(expires_at, &None, "sending both would coalesce it back");
            }
            other => panic!("unexpected mutation: {other:?}"),
        }
    }

    #[tokio::test]
    async fn deleting_an_unsent_share_drops_its_queued_creation() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let id = create_share_in_tx(&mut conn, &["t1".into()], None, None)
            .await
            .unwrap();

        delete_share_in_tx(&mut conn, &id).await.unwrap();

        assert_eq!(count(&mut conn, "SELECT count(*) FROM remote_share").await, 0);
        assert!(pending(&mut conn, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn every_gesture_leaves_the_projection_and_the_queue_in_step() {
        // A sweep rather than a per-gesture assertion: any future
        // gesture that writes one half and forgets the other fails here.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();

        set_favorite_in_tx(&mut conn, "track", "t1", true).await.unwrap();
        set_rating_in_tx(&mut conn, "track", "t1", 3).await.unwrap();
        let playlist = create_playlist_in_tx(&mut conn, "Mix", &[]).await.unwrap();
        update_playlist_in_tx(&mut conn, &playlist, Some("Renamed".into()), None, None, false)
            .await
            .unwrap();
        scrobble_in_tx(&mut conn, "t1", true, Some(1_000)).await.unwrap();
        save_queue_in_tx(&mut conn, &["t1".into()], None, 0, None)
            .await
            .unwrap();
        create_share_in_tx(&mut conn, &["t1".into()], None, None)
            .await
            .unwrap();

        let queued = pending(&mut conn, 20).await.unwrap();
        assert_eq!(queued.len(), 7, "a gesture wrote without queuing");
        let rows = count(&mut conn, "SELECT count(*) FROM remote_favorite").await
            + count(&mut conn, "SELECT count(*) FROM remote_rating").await
            + count(&mut conn, "SELECT count(*) FROM remote_playlist").await
            + count(&mut conn, "SELECT count(*) FROM remote_history").await
            + count(&mut conn, "SELECT count(*) FROM remote_queue").await
            + count(&mut conn, "SELECT count(*) FROM remote_share").await;
        assert_eq!(rows, 6, "a gesture queued without writing");
    }
}
