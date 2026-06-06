//! Per-track snapshots for outbound `playlist + field: "tracks"` ops
//! (Phase 1.j.b — wire bump to populate the server's
//! `playlist_track.snapshot_*` columns).
//!
//! ## Why a snapshot
//!
//! The `track_id` field in the outbound payload is the SOURCE
//! desktop's local-i64 id. The server can't resolve it cross-device
//! (a track with id=42 on device A is unrelated to id=42 on device
//! B), so a remote viewer would see only an opaque integer. The
//! snapshot carries the displayable columns (`title`, `artist`,
//! `duration_ms`) alongside the id, which is what the server's
//! [`db::playlist_track`](https://github.com/InstaZDLL/waveflow-server/blob/main/src/db.rs)
//! materialiser stores and what the public share preview at
//! `/api/v1/share/playlists/{token}` renders to the wider web.
//!
//! ## Wire shape
//!
//! Returns a JSON object keyed by track id as a STRING (JSON object
//! keys can't be ints, and the server-side parser does the same
//! conversion):
//!
//! ```json
//! { "42": { "title": "One More Time", "artist": "Daft Punk", "duration_ms": 320000 },
//!   "43": { "title": "Around the World", "duration_ms": 280000 } }
//! ```
//!
//! Empty input → empty object. A track id whose row was deleted
//! between the playlist write and the snapshot SELECT is silently
//! dropped from the map; the server's apply pipeline tolerates the
//! id-without-snapshot case (the row lands NULL-snapshot and is
//! invisible to the public preview until the next sync re-emits).
//!
//! ## Atomicity
//!
//! Called from inside the same `&mut SqliteConnection` transaction
//! as the playlist write + the outbox enqueue. The SELECT is a
//! pure read against the `track` + `track_artist` tables — neither
//! is mutated by the playlist path — so the snapshot reflects the
//! state the user just acted on without an extra pool acquire.

use serde_json::{json, Map, Value};
use sqlx::SqliteConnection;

use crate::error::AppResult;

/// Build the `snapshots` payload for a batch of track ids. Always
/// returns a JSON object — empty when `track_ids` is empty, partial
/// when some ids resolve to no row. The caller folds the result
/// into the outbound payload alongside `track_ids`.
pub async fn build_snapshots(conn: &mut SqliteConnection, track_ids: &[i64]) -> AppResult<Value> {
    if track_ids.is_empty() {
        return Ok(Value::Object(Map::new()));
    }

    // Build the IN-clause placeholders. We can't bind a slice
    // directly to SQLite — sqlx 0.9 has no `Encode for Vec<i64>` on
    // the SQLite backend — so we expand `?, ?, ?, …` and bind one
    // by one. The id list comes from server-trusted internal state
    // (the caller already validated it against the playlist), so
    // the placeholder count is the upper bound on the SQL string
    // size, not the user input.
    // `iter::repeat().take(n)` rather than the newer `repeat_n`
    // helper — the latter is stable only since Rust 1.82 and the
    // repo's MSRV is 1.80. Mirrors the same pattern in
    // commands/radio.rs:203.
    let placeholders = std::iter::repeat("?")
        .take(track_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT t.id, t.title, t.duration_ms,
                (SELECT GROUP_CONCAT(name, ', ') FROM (
                    SELECT ar2.name FROM track_artist ta2
                    JOIN artist ar2 ON ar2.id = ta2.artist_id
                    WHERE ta2.track_id = t.id
                    ORDER BY ta2.position
                )) AS artist_name
           FROM track t
          WHERE t.id IN ({placeholders})"
    );

    // `AssertSqlSafe` is the repo's audited path for dynamic
    // `IN (?, ?, …)` expansions (see commands/radio.rs:225 for the
    // mirror pattern). The placeholder string is built from
    // `track_ids.len()` only, so user input never reaches the SQL
    // text — only the bind values.
    let mut query =
        sqlx::query_as::<_, (i64, String, i64, Option<String>)>(sqlx::AssertSqlSafe(sql));
    for id in track_ids {
        query = query.bind(*id);
    }
    let rows = query.fetch_all(&mut *conn).await?;

    let mut snapshots = Map::with_capacity(rows.len());
    for (id, title, duration_ms, artist) in rows {
        let mut entry = Map::new();
        entry.insert("title".to_string(), Value::String(title));
        if let Some(a) = artist {
            entry.insert("artist".to_string(), Value::String(a));
        }
        entry.insert("duration_ms".to_string(), json!(duration_ms));
        snapshots.insert(id.to_string(), Value::Object(entry));
    }
    Ok(Value::Object(snapshots))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    /// Bootstrap a minimal `track` + `artist` + `track_artist` schema
    /// for the snapshot SELECT. We don't run the full migration set —
    /// just the two tables the query touches — to keep the test
    /// surface tight. A future repo-wide test harness can replace
    /// this with `sqlx::test`.
    async fn setup(pool: &SqlitePool) {
        sqlx::query(
            "CREATE TABLE track (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                duration_ms INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("CREATE TABLE artist (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE track_artist (
                track_id INTEGER NOT NULL,
                artist_id INTEGER NOT NULL,
                position INTEGER NOT NULL,
                PRIMARY KEY (track_id, artist_id)
            )",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn empty_input_yields_empty_object() {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let out = build_snapshots(&mut conn, &[]).await.unwrap();
        assert_eq!(out, json!({}));
    }

    #[tokio::test]
    async fn populates_title_artist_duration() {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        setup(&pool).await;
        sqlx::query(
            "INSERT INTO track (id, title, duration_ms) VALUES (1, 'One More Time', 320000)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO artist (id, name) VALUES (10, 'Daft Punk')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO track_artist (track_id, artist_id, position) VALUES (1, 10, 0)")
            .execute(&pool)
            .await
            .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let out = build_snapshots(&mut conn, &[1]).await.unwrap();
        assert_eq!(
            out,
            json!({
                "1": { "title": "One More Time", "artist": "Daft Punk", "duration_ms": 320000 }
            })
        );
    }

    #[tokio::test]
    async fn artist_collapses_multi_with_join_string() {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        setup(&pool).await;
        sqlx::query("INSERT INTO track (id, title, duration_ms) VALUES (2, 'Get Lucky', 369000)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO artist (id, name) VALUES (10, 'Daft Punk'), (11, 'Pharrell Williams')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track_artist (track_id, artist_id, position) VALUES (2, 10, 0), (2, 11, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let out = build_snapshots(&mut conn, &[2]).await.unwrap();
        let entry = out.get("2").unwrap();
        assert_eq!(
            entry["artist"].as_str(),
            Some("Daft Punk, Pharrell Williams")
        );
    }

    #[tokio::test]
    async fn missing_track_id_silently_dropped() {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        setup(&pool).await;
        sqlx::query("INSERT INTO track (id, title, duration_ms) VALUES (5, 'A', 100)")
            .execute(&pool)
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        // Ask for 5 (exists) + 999 (doesn't). Output covers only 5.
        let out = build_snapshots(&mut conn, &[5, 999]).await.unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert!(obj.contains_key("5"));
    }

    #[tokio::test]
    async fn artist_absent_yields_no_artist_field() {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        setup(&pool).await;
        sqlx::query("INSERT INTO track (id, title, duration_ms) VALUES (3, 'Untitled', 60000)")
            .execute(&pool)
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let out = build_snapshots(&mut conn, &[3]).await.unwrap();
        let entry = out.get("3").unwrap().as_object().unwrap();
        assert_eq!(
            entry.get("title").and_then(|v| v.as_str()),
            Some("Untitled")
        );
        assert_eq!(
            entry.get("duration_ms").and_then(|v| v.as_i64()),
            Some(60000)
        );
        assert!(
            !entry.contains_key("artist"),
            "artist must be omitted, not null"
        );
    }
}
