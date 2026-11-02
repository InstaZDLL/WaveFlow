//! The `network_source` table.

use sqlx::{Row, SqlitePool};

use crate::error::AppResult;

use super::cache;

/// The cache budget's `profile_setting`. Written by the settings card, read
/// here because the decoder thread has no database to ask.
pub const CACHE_MAX_GB_KEY: &str = "network.cache_max_gb";

/// One source, secret included. Never serialized: what reaches the webview is
/// [`crate::commands::network_library::NetworkSourceView`].
#[derive(Clone)]
pub struct SourceRow {
    pub id: i64,
    pub uuid: String,
    pub kind: String,
    pub name: String,
    pub endpoint: String,
    pub share: String,
    pub username: Option<String>,
    pub secret: Option<String>,
    pub allow_plain_http: bool,
}

impl std::fmt::Debug for SourceRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceRow")
            .field("id", &self.id)
            .field("uuid", &self.uuid)
            .field("kind", &self.kind)
            .field("endpoint", &self.endpoint)
            .field("share", &self.share)
            .field("has_secret", &self.secret.is_some())
            .finish_non_exhaustive()
    }
}

fn from_row(row: sqlx::sqlite::SqliteRow) -> Result<SourceRow, sqlx::Error> {
    Ok(SourceRow {
        id: row.try_get("id")?,
        uuid: row.try_get("uuid")?,
        kind: row.try_get("kind")?,
        name: row.try_get("name")?,
        endpoint: row.try_get("endpoint")?,
        share: row.try_get("share")?,
        username: row.try_get("username")?,
        secret: row.try_get("secret")?,
        allow_plain_http: row.try_get::<i64, _>("allow_plain_http")? != 0,
    })
}

const COLUMNS: &str = "id, uuid, kind, name, endpoint, share, username, secret, allow_plain_http";

pub async fn list(pool: &SqlitePool) -> AppResult<Vec<SourceRow>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM network_source ORDER BY name COLLATE NOCASE"
    )))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()?)
}

pub async fn get(pool: &SqlitePool, id: i64) -> AppResult<Option<SourceRow>> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM network_source WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(from_row).transpose()?)
}

/// The source for this address, when it already has one: a second folder of
/// the same share belongs to the same source, which is also what makes the
/// overlap check able to compare the two.
pub async fn find_same(
    pool: &SqlitePool,
    kind: &str,
    endpoint: &str,
    share: &str,
) -> AppResult<Option<SourceRow>> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM network_source WHERE kind = ? AND endpoint = ? AND share = ?"
    )))
    .bind(kind)
    .bind(endpoint)
    .bind(share)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(from_row).transpose()?)
}

pub async fn insert(pool: &SqlitePool, row: &SourceRow) -> AppResult<i64> {
    let id = sqlx::query(
        "INSERT INTO network_source
            (uuid, kind, name, endpoint, share, username, secret, allow_plain_http, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.uuid)
    .bind(&row.kind)
    .bind(&row.name)
    .bind(&row.endpoint)
    .bind(&row.share)
    .bind(row.username.as_deref())
    .bind(row.secret.as_deref())
    .bind(i64::from(row.allow_plain_http))
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

pub async fn update_credentials(
    pool: &SqlitePool,
    id: i64,
    username: Option<&str>,
    secret: Option<&str>,
) -> AppResult<()> {
    let mut conn = pool.acquire().await?;
    forget_on_delete(&mut conn).await?;
    sqlx::query("UPDATE network_source SET username = ?, secret = ? WHERE id = ?")
        .bind(username)
        .bind(secret)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The old password must not survive in a free page, where a copy of the file
/// would carry it: `profile_io` only blanks the rows it finds.
async fn forget_on_delete(conn: &mut sqlx::SqliteConnection) -> AppResult<()> {
    sqlx::query("PRAGMA secure_delete = ON")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, id: i64) -> AppResult<()> {
    let mut conn = pool.acquire().await?;
    forget_on_delete(&mut conn).await?;
    sqlx::query("DELETE FROM network_source WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The ids of the library folders on a source.
pub async fn folders_of(pool: &SqlitePool, uuid: &str) -> AppResult<Vec<(i64, String)>> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, path FROM library_folder WHERE path LIKE 'wfnet://%'")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, path)| {
            waveflow_core::network_locator::parse(path).is_some_and(|p| p.source == uuid)
        })
        .collect())
}

/// The cache budget in GiB, clamped: a hand-edited row must not empty the
/// cache or fill the disk.
pub async fn cache_max_gb(pool: &SqlitePool) -> u64 {
    sqlx::query_scalar::<_, String>("SELECT value FROM profile_setting WHERE key = ?")
        .bind(CACHE_MAX_GB_KEY)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(cache::DEFAULT_MAX_GB)
        .clamp(cache::MIN_MAX_GB, cache::MAX_MAX_GB)
}
