//! Albums, artists and playlists pinned to the sidebar.
//!
//! One row of `sidebar_pin` per pin, pointing at exactly one album, artist
//! or playlist; the sidebar lists them in the order they were pinned. A pin
//! follows its target out through `ON DELETE CASCADE`, so deleting a
//! playlist or an artist needs no cleanup here. An album merged into
//! another is the exception: [`carry_album_pin`] moves its pin onto the
//! survivor before the delete would cascade it away.

use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::CoreResult;

/// What a pin points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    Album,
    Artist,
    Playlist,
}

impl PinKind {
    /// The `sidebar_pin` column holding this kind's target. A fixed
    /// string per variant, so it can be spliced into SQL.
    fn column(self) -> &'static str {
        match self {
            PinKind::Album => "album_id",
            PinKind::Artist => "artist_id",
            PinKind::Playlist => "playlist_id",
        }
    }
}

/// Pin `id`. Pinning what is already pinned keeps the first pin, and
/// with it the place it holds in the list.
pub async fn pin(pool: &SqlitePool, kind: PinKind, id: i64, now_ms: i64) -> CoreResult<()> {
    let sql = format!(
        "INSERT OR IGNORE INTO sidebar_pin ({}, pinned_at) VALUES (?, ?)",
        kind.column()
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .bind(now_ms)
        .execute(pool)
        .await?;
    Ok(())
}

/// Unpin `id`. Returns whether there was a pin to remove.
pub async fn unpin(pool: &SqlitePool, kind: PinKind, id: i64) -> CoreResult<bool> {
    let sql = format!("DELETE FROM sidebar_pin WHERE {} = ?", kind.column());
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Move the pin of album `from` onto album `into`, when two albums are
/// merged and `from` is about to be deleted. Without it the cascade would
/// drop the pin with the album the user pinned, though the record is
/// still there under the survivor's id. When `into` is pinned already,
/// that pin stays and `from`'s goes with its album.
pub async fn carry_album_pin(conn: &mut SqliteConnection, from: i64, into: i64) -> CoreResult<()> {
    sqlx::query("UPDATE OR IGNORE sidebar_pin SET album_id = ? WHERE album_id = ?")
        .bind(into)
        .bind(from)
        .execute(conn)
        .await?;
    Ok(())
}
