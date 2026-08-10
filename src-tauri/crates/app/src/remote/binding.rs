//! Which server the active profile talks to, and where it is up to.
//!
//! One row, pinned by `CHECK (id = 1)` in the migration: a profile
//! binds to exactly one server account (RFC-005 §Decision 3). The
//! binding is per-profile rather than app-wide — which is where v1 kept
//! the server URL — because with a universal client two profiles may
//! legitimately point at two different servers, and a cursor is
//! meaningless across accounts.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};

use crate::error::{AppError, AppResult};

/// What kind of server sits behind the URL, and therefore which
/// identity fields carry meaning.
///
/// A third-party Subsonic server has no account UUID, no device notion
/// and no journal cursor. Modelling that as one struct with three
/// optional fields would spread `unwrap` over cases that cannot occur;
/// the enum makes the impossible states unrepresentable instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "flavour", rename_all = "lowercase")]
pub enum RemoteIdentity {
    /// Native WaveFlow account. The only branch with a journal.
    Waveflow {
        /// The account the tokens belong to. Opaque to us beyond
        /// equality — used to detect that the user signed into a
        /// *different* account, which invalidates the whole projection.
        account_id: String,
        /// The non-revoked device this install registered. Sent on
        /// every mutation and used as the ACK key; the server rejects a
        /// device owned by another account.
        device_id: String,
        /// Journal position. `0` means "never bootstrapped".
        cursor: i64,
    },
    /// Any server speaking the Subsonic protocol.
    Subsonic { username: String },
}

impl RemoteIdentity {
    /// Canonical TEXT for the `flavour` column. Stable once persisted —
    /// changing a value would need a migration.
    pub fn flavour(&self) -> &'static str {
        match self {
            RemoteIdentity::Waveflow { .. } => "waveflow",
            RemoteIdentity::Subsonic { .. } => "subsonic",
        }
    }

    /// The journal position, or `None` for a server that has no
    /// journal. Callers must treat `None` as "this capability does not
    /// exist here", never as "start from zero".
    pub fn cursor(&self) -> Option<i64> {
        match self {
            RemoteIdentity::Waveflow { cursor, .. } => Some(*cursor),
            RemoteIdentity::Subsonic { .. } => None,
        }
    }

    /// The device identifier to stamp mutations with, when there is one.
    pub fn device_id(&self) -> Option<&str> {
        match self {
            RemoteIdentity::Waveflow { device_id, .. } => Some(device_id),
            RemoteIdentity::Subsonic { .. } => None,
        }
    }
}

/// The persisted binding row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteBinding {
    /// Base URL, without a trailing slash.
    pub server_url: String,
    pub identity: RemoteIdentity,
    /// Optional library filter. An account with access to several
    /// libraries picks one rather than spawning artificial profiles.
    pub active_library_id: Option<String>,
    /// `None` until the first snapshot has been applied.
    ///
    /// This is what separates "no data because we never asked" from "no
    /// data because the account is empty" — without it the two are
    /// indistinguishable, and a failed bootstrap would present as a
    /// successfully synchronized empty account.
    pub bootstrapped_at: Option<i64>,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Read the binding, or `None` when this profile is not bound to any
/// server. Local-only is the normal case, not an error.
pub async fn read(conn: &mut SqliteConnection) -> AppResult<Option<RemoteBinding>> {
    let row = sqlx::query(
        "SELECT server_url, flavour, account_id, device_id, username,
                cursor, active_library_id, bootstrapped_at
           FROM remote_binding WHERE id = 1",
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else { return Ok(None) };

    let flavour: String = row.try_get("flavour")?;
    let identity = match flavour.as_str() {
        "waveflow" => RemoteIdentity::Waveflow {
            // The migration's CHECK guarantees both columns are present
            // on this branch, so a NULL here is a corrupt row rather
            // than a state the protocol allows.
            account_id: row.try_get("account_id")?,
            device_id: row.try_get("device_id")?,
            cursor: row.try_get("cursor")?,
        },
        "subsonic" => RemoteIdentity::Subsonic {
            username: row.try_get("username")?,
        },
        other => {
            return Err(AppError::Other(format!(
                "remote_binding.flavour holds an unknown value {other:?}"
            )))
        }
    };

    Ok(Some(RemoteBinding {
        server_url: row.try_get("server_url")?,
        identity,
        active_library_id: row.try_get("active_library_id")?,
        bootstrapped_at: row.try_get("bootstrapped_at")?,
    }))
}

/// Create or replace the binding.
///
/// Replacing is a deliberate whole-row overwrite rather than a merge:
/// binding to a different server, or to a different account on the same
/// server, must not inherit the previous cursor. Callers that rebind are
/// responsible for clearing the projection in the same transaction —
/// a cursor from another account would otherwise be read as a valid
/// position in a journal it never belonged to.
pub async fn write(conn: &mut SqliteConnection, binding: &RemoteBinding) -> AppResult<()> {
    let now = now_ms();
    let (account_id, device_id, cursor, username) = match &binding.identity {
        RemoteIdentity::Waveflow {
            account_id,
            device_id,
            cursor,
        } => (
            Some(account_id.as_str()),
            Some(device_id.as_str()),
            *cursor,
            None,
        ),
        RemoteIdentity::Subsonic { username } => (None, None, 0, Some(username.as_str())),
    };

    sqlx::query(
        "INSERT INTO remote_binding
            (id, server_url, flavour, account_id, device_id, username,
             cursor, active_library_id, bootstrapped_at, created_at, updated_at)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
            server_url        = excluded.server_url,
            flavour           = excluded.flavour,
            account_id        = excluded.account_id,
            device_id         = excluded.device_id,
            username          = excluded.username,
            cursor            = excluded.cursor,
            active_library_id = excluded.active_library_id,
            bootstrapped_at   = excluded.bootstrapped_at,
            updated_at        = excluded.updated_at",
    )
    .bind(binding.server_url.trim_end_matches('/'))
    .bind(binding.identity.flavour())
    .bind(account_id)
    .bind(device_id)
    .bind(username)
    .bind(cursor)
    .bind(binding.active_library_id.as_deref())
    .bind(binding.bootstrapped_at)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Advance the journal cursor.
///
/// Monotonic by construction: `MAX(cursor, ?)` in SQL rather than a
/// read-modify-write in Rust. Two passes can legitimately finish out of
/// order — a socket-triggered catch-up racing a periodic one — and the
/// lower of the two must not drag the position backwards, which would
/// replay events the client has already applied.
pub async fn advance_cursor(conn: &mut SqliteConnection, cursor: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE remote_binding
            SET cursor = MAX(cursor, ?), updated_at = ?
          WHERE id = 1 AND flavour = 'waveflow'",
    )
    .bind(cursor)
    .bind(now_ms())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Stamp the binding as bootstrapped, and pin the cursor the snapshot
/// was consistent at.
///
/// The cursor is set outright here, not `MAX`-ed: a snapshot is a
/// whole-projection replacement, so its cursor is authoritative even if
/// it is lower than a position reached before the projection was
/// discarded. That case is exactly the recovery path — a client that
/// could not apply a known event throws its projection away and starts
/// again from a fresh snapshot.
pub async fn mark_bootstrapped(conn: &mut SqliteConnection, cursor: i64) -> AppResult<()> {
    let now = now_ms();
    sqlx::query(
        "UPDATE remote_binding
            SET cursor = ?, bootstrapped_at = ?, updated_at = ?
          WHERE id = 1 AND flavour = 'waveflow'",
    )
    .bind(cursor)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Drop the binding. Leaves the projection and the outbound queue
/// alone — the caller decides whether unbinding also discards pending
/// writes, which is a user-visible choice, not a storage detail.
pub async fn clear(conn: &mut SqliteConnection) -> AppResult<()> {
    sqlx::query("DELETE FROM remote_binding WHERE id = 1")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    /// Real schema, not a hand-rolled fixture: the CHECK constraints are
    /// half of what this module relies on, and a fixture without them
    /// would prove nothing.
    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../../migrations/profile/20260810120000_remote_source_projection.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn waveflow_binding(cursor: i64) -> RemoteBinding {
        RemoteBinding {
            server_url: "https://music.example".into(),
            identity: RemoteIdentity::Waveflow {
                account_id: "acc-1".into(),
                device_id: "dev-1".into(),
                cursor,
            },
            active_library_id: None,
            bootstrapped_at: None,
        }
    }

    #[tokio::test]
    async fn unbound_profile_reads_as_none() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(read(&mut conn).await.unwrap(), None);
    }

    #[tokio::test]
    async fn waveflow_binding_round_trips() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let binding = waveflow_binding(7);
        write(&mut conn, &binding).await.unwrap();
        assert_eq!(read(&mut conn).await.unwrap(), Some(binding));
    }

    #[tokio::test]
    async fn subsonic_binding_round_trips_without_a_cursor() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let binding = RemoteBinding {
            server_url: "https://other.example".into(),
            identity: RemoteIdentity::Subsonic {
                username: "bob".into(),
            },
            active_library_id: Some("lib-3".into()),
            bootstrapped_at: None,
        };
        write(&mut conn, &binding).await.unwrap();
        let read_back = read(&mut conn).await.unwrap().unwrap();
        assert_eq!(read_back, binding);
        assert_eq!(read_back.identity.cursor(), None);
        assert_eq!(read_back.identity.device_id(), None);
    }

    #[tokio::test]
    async fn trailing_slash_is_stripped_so_paths_never_double_up() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut binding = waveflow_binding(0);
        binding.server_url = "https://music.example/".into();
        write(&mut conn, &binding).await.unwrap();
        assert_eq!(
            read(&mut conn).await.unwrap().unwrap().server_url,
            "https://music.example"
        );
    }

    #[tokio::test]
    async fn a_late_lower_cursor_does_not_drag_the_position_backwards() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        write(&mut conn, &waveflow_binding(0)).await.unwrap();

        advance_cursor(&mut conn, 42).await.unwrap();
        // A catch-up pass that started earlier finishing later.
        advance_cursor(&mut conn, 11).await.unwrap();

        assert_eq!(read(&mut conn).await.unwrap().unwrap().identity.cursor(), Some(42));
    }

    #[tokio::test]
    async fn a_snapshot_pins_its_cursor_even_when_lower() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        write(&mut conn, &waveflow_binding(0)).await.unwrap();
        advance_cursor(&mut conn, 42).await.unwrap();

        // The recovery path: the projection was discarded, so the
        // position it had reached is meaningless.
        mark_bootstrapped(&mut conn, 11).await.unwrap();

        let binding = read(&mut conn).await.unwrap().unwrap();
        assert_eq!(binding.identity.cursor(), Some(11));
        assert!(binding.bootstrapped_at.is_some());
    }

    #[tokio::test]
    async fn rebinding_replaces_rather_than_merges() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        write(&mut conn, &waveflow_binding(0)).await.unwrap();
        advance_cursor(&mut conn, 99).await.unwrap();

        // Signing into a different account must not inherit a cursor
        // into a journal it never belonged to.
        write(&mut conn, &waveflow_binding(0)).await.unwrap();
        assert_eq!(read(&mut conn).await.unwrap().unwrap().identity.cursor(), Some(0));
    }

    #[tokio::test]
    async fn advancing_a_subsonic_binding_is_a_no_op() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        write(
            &mut conn,
            &RemoteBinding {
                server_url: "https://other.example".into(),
                identity: RemoteIdentity::Subsonic {
                    username: "bob".into(),
                },
                active_library_id: None,
                bootstrapped_at: None,
            },
        )
        .await
        .unwrap();

        advance_cursor(&mut conn, 42).await.unwrap();
        mark_bootstrapped(&mut conn, 42).await.unwrap();

        let binding = read(&mut conn).await.unwrap().unwrap();
        assert_eq!(binding.identity.cursor(), None);
        assert_eq!(binding.bootstrapped_at, None);
    }
}
