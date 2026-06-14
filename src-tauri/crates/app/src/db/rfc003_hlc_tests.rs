//! Smoke tests for the RFC-003 §3.1 HLC migrations (Phase A.3).
//!
//! Runs the compiled-in migrators against fresh in-memory SQLite DBs
//! and asserts that every entity row has the four HLC columns and that
//! `metadata_digest_version` is seeded with the expected entity names.
//!
//! Catches three regression classes the dual-shape ingest on the
//! server side relies on:
//!
//! - Column NAMES match the server schema (`hlc_wall`, `hlc_logical`,
//!   `origin_device_id`, `payload_hash`). The A.4 emit path will read
//!   these by name; a typo here only surfaces at run-time otherwise.
//! - Columns are present on every entity the desktop pushes today
//!   (`library` / `track` / `playlist` / `playlist_track` /
//!   `liked_track` per profile DB + `profile` in app.db). The
//!   `track` row also carries a `rating_` mirror because rating is a
//!   column on `track` rather than a sibling table.
//! - `metadata_digest_version` is seeded with the right entity names
//!   so the A.4 bump can `INSERT ... ON CONFLICT DO UPDATE` without
//!   needing a separate provisioning step.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

/// HLC + payload_hash quartet the migration adds to every synced entity.
const HLC_COLUMNS: &[(&str, &str, i64)] = &[
    ("hlc_wall", "INTEGER", 1),
    ("hlc_logical", "INTEGER", 1),
    ("origin_device_id", "TEXT", 0),
    ("payload_hash", "BLOB", 0),
];

async fn fresh_pool() -> SqlitePool {
    let opts = SqliteConnectOptions::from_str(":memory:")
        .unwrap()
        .foreign_keys(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap()
}

async fn column_info(pool: &SqlitePool, table: &str, column: &str) -> Option<(String, i64)> {
    let stmt = format!("PRAGMA table_info({table})");
    let rows = sqlx::query(sqlx::AssertSqlSafe(stmt))
        .fetch_all(pool)
        .await
        .unwrap();
    for row in rows {
        let name: String = row.get("name");
        if name == column {
            let ty: String = row.get("type");
            let notnull: i64 = row.get("notnull");
            return Some((ty, notnull));
        }
    }
    None
}

async fn assert_hlc_quartet(pool: &SqlitePool, table: &str) {
    for (col, ty, notnull) in HLC_COLUMNS {
        let info = column_info(pool, table, col)
            .await
            .unwrap_or_else(|| panic!("missing column {table}.{col}"));
        assert_eq!(info.0.to_uppercase(), *ty, "{table}.{col} type mismatch");
        assert_eq!(info.1, *notnull, "{table}.{col} notnull flag mismatch");
    }
}

#[tokio::test]
async fn profile_migrations_apply_hlc_quartet_to_every_entity() {
    let pool = fresh_pool().await;
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();

    for table in [
        "library",
        "track",
        "playlist",
        "playlist_track",
        "liked_track",
    ] {
        assert_hlc_quartet(&pool, table).await;
    }

    // Rating mirror lives on `track` because rating is a column.
    for col in [
        "rating_hlc_wall",
        "rating_hlc_logical",
        "rating_origin_device_id",
        "rating_payload_hash",
    ] {
        assert!(
            column_info(&pool, "track", col).await.is_some(),
            "missing track.{col}"
        );
    }
}

#[tokio::test]
async fn profile_migrations_seed_metadata_digest_version() {
    let pool = fresh_pool().await;
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();

    let mut entities: Vec<String> =
        sqlx::query_scalar("SELECT entity FROM metadata_digest_version ORDER BY entity")
            .fetch_all(&pool)
            .await
            .unwrap();
    entities.sort();

    let mut expected: Vec<String> = [
        "library",
        "liked_track",
        "playlist",
        "playlist_track",
        "track",
        "track_rating",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    expected.sort();

    assert_eq!(entities, expected);

    // Versions all start at 0 so the first bump lands at 1.
    let versions: Vec<i64> = sqlx::query_scalar("SELECT version FROM metadata_digest_version")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(versions.iter().all(|v| *v == 0));
}

#[tokio::test]
async fn app_migrations_apply_hlc_quartet_to_profile() {
    let pool = fresh_pool().await;
    sqlx::migrate!("../../migrations/app")
        .run(&pool)
        .await
        .unwrap();

    assert_hlc_quartet(&pool, "profile").await;
}

#[tokio::test]
async fn app_migrations_seed_metadata_digest_version_with_profile_only() {
    let pool = fresh_pool().await;
    sqlx::migrate!("../../migrations/app")
        .run(&pool)
        .await
        .unwrap();

    let entities: Vec<String> =
        sqlx::query_scalar("SELECT entity FROM metadata_digest_version ORDER BY entity")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(entities, vec!["profile".to_string()]);
}
