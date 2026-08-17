//! Access / refresh token persistence for the remote binding.
//!
//! Kept apart from [`super::binding`] because the two have different
//! lifetimes: the binding changes when the user picks a server, the
//! tokens change every fifteen minutes.
//!
//! ## These are not JWTs
//!
//! The v1 client validated its stored credential by checking it had
//! three dot-separated segments, because Better Auth minted JWTs. The v2
//! server issues **opaque** tokens — `wfa_…` for access, `wfr_…` for
//! refresh, `wfapi_…` for long-lived API tokens — and hashes them
//! server-side. The v1 check would reject every single one of them, so
//! this module deliberately validates nothing beyond non-emptiness:
//! whether a token is good is a question only the server can answer, and
//! it answers it on the first request.
//!
//! ## Rotation is what makes this delicate
//!
//! A refresh returns a *new* refresh token and kills the old one. Two
//! concurrent refreshes therefore do not merely duplicate work — the
//! second presents a token the first has already spent, fails, and can
//! leave the profile signed out with a perfectly valid session on the
//! server. Refreshing is single-flighted in [`super::client`]; this
//! module's job is to make the write atomic so a crash between the two
//! tokens cannot strand the pair.
//!
//! ## Storage
//!
//! `auth_credential` under the existing `waveflow_server` provider —
//! the same row family as the other integrations, already allowed by the
//! table's CHECK. The column names say `*_encrypted`; as in v1 the bytes
//! are stored raw, and moving the pair into the OS keyring is a
//! hardening pass that will not change this module's surface.

use chrono::Utc;
use sqlx::{Row, SqliteConnection};

use crate::error::{AppError, AppResult};

/// `auth_credential.provider` value. Must stay in the CHECK constraint
/// installed by `20260601000000_waveflow_server_auth_provider`.
pub const PROVIDER: &str = "waveflow_server";

/// A live credential pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    /// Epoch milliseconds at which the access token stops being
    /// accepted, or `None` when the server did not say.
    pub expires_at: Option<i64>,
    /// The account's username. Persisted only so the Settings card can
    /// name the signed-in account without a round-trip.
    pub username: Option<String>,
}

impl TokenPair {
    /// Whether the access token should be refreshed before use.
    ///
    /// The margin matters: a token that expires in two seconds passes a
    /// naive `now < expires_at` test and then 401s in flight. Refreshing
    /// slightly early costs one extra round-trip; refreshing too late
    /// costs a failed request and a retry on every call made near the
    /// boundary.
    pub fn needs_refresh(&self, now_ms: i64, margin_ms: i64) -> bool {
        match self.expires_at {
            Some(expires_at) => now_ms + margin_ms >= expires_at,
            // No expiry recorded — a long-lived API token, or a server
            // that declined to say. Assume it is good and let a 401
            // drive the refresh instead of guessing.
            None => false,
        }
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Read the stored pair, or `None` when this profile is signed out.
pub async fn read(conn: &mut SqliteConnection) -> AppResult<Option<TokenPair>> {
    let row = sqlx::query(
        "SELECT token_encrypted, refresh_token_encrypted, expires_at, username
           FROM auth_credential WHERE provider = ?",
    )
    .bind(PROVIDER)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else { return Ok(None) };

    let access: Vec<u8> = row.try_get("token_encrypted")?;
    let refresh: Option<Vec<u8>> = row.try_get("refresh_token_encrypted")?;

    let access_token = decode(access, "access token")?;
    // A row with no refresh token is a legacy v1 credential, or a
    // long-lived API token pasted by hand. Both are unusable for the v2
    // flow, which cannot recover the session once the access token
    // lapses — treating it as signed out is the honest answer.
    let Some(refresh_token) = refresh.map(|bytes| decode(bytes, "refresh token")).transpose()?
    else {
        return Ok(None);
    };

    if access_token.is_empty() || refresh_token.is_empty() {
        return Ok(None);
    }

    Ok(Some(TokenPair {
        access_token,
        refresh_token,
        expires_at: row.try_get("expires_at")?,
        username: row.try_get("username")?,
    }))
}

fn decode(bytes: Vec<u8>, what: &str) -> AppResult<String> {
    String::from_utf8(bytes)
        .map(|value| value.trim().to_string())
        .map_err(|_| AppError::Other(format!("stored {what} is not valid UTF-8")))
}

/// Persist a pair, replacing whatever was there.
///
/// Both tokens land in one statement on purpose. Writing them
/// separately would leave a window where a crash strands a new access
/// token next to a refresh token that has already been spent — the
/// profile would look signed in and be unable to recover.
///
/// SECURITY: the tokens are currently stored **in cleartext** despite the
/// `*_encrypted` column names — no encryption is applied here yet. The
/// refresh token is the sensitive one (it re-mints access tokens). Moving
/// this to the OS keyring is tracked as hardening work, not done in this
/// change.
pub async fn write(conn: &mut SqliteConnection, pair: &TokenPair) -> AppResult<()> {
    if pair.access_token.trim().is_empty() || pair.refresh_token.trim().is_empty() {
        return Err(AppError::Other(
            "refusing to store an empty token pair".into(),
        ));
    }
    let now = now_ms();
    sqlx::query(
        "INSERT INTO auth_credential
            (provider, username, token_encrypted, refresh_token_encrypted,
             expires_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(provider) DO UPDATE SET
            username                = excluded.username,
            token_encrypted         = excluded.token_encrypted,
            refresh_token_encrypted = excluded.refresh_token_encrypted,
            expires_at              = excluded.expires_at,
            updated_at              = excluded.updated_at",
    )
    .bind(PROVIDER)
    .bind(pair.username.as_deref())
    .bind(pair.access_token.trim().as_bytes())
    .bind(pair.refresh_token.trim().as_bytes())
    .bind(pair.expires_at)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Turn the server's `expires_in` (seconds) into an absolute deadline.
///
/// Absolute rather than relative because the value has to survive a
/// restart: a stored duration says nothing about how much of it is left.
pub fn deadline_from_expires_in(expires_in_secs: i64) -> Option<i64> {
    if expires_in_secs <= 0 {
        return None;
    }
    expires_in_secs
        .checked_mul(1_000)
        .and_then(|ms| now_ms().checked_add(ms))
}

/// Sign out. Idempotent.
pub async fn clear(conn: &mut SqliteConnection) -> AppResult<()> {
    sqlx::query("DELETE FROM auth_credential WHERE provider = ?")
        .bind(PROVIDER)
        .execute(&mut *conn)
        .await?;
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
        // The real table, CHECK included: storing under a provider the
        // constraint does not allow is exactly the kind of failure a
        // hand-rolled fixture would hide.
        sqlx::raw_sql(
            "CREATE TABLE auth_credential (
                provider                TEXT PRIMARY KEY
                                        CHECK (provider IN ('lastfm','listenbrainz','deezer','spotify','waveflow_server')),
                username                TEXT,
                token_encrypted         BLOB NOT NULL,
                refresh_token_encrypted BLOB,
                expires_at              INTEGER,
                created_at              INTEGER NOT NULL,
                updated_at              INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn pair() -> TokenPair {
        TokenPair {
            access_token: "wfa_abc".into(),
            refresh_token: "wfr_def".into(),
            expires_at: Some(1_000_000),
            username: Some("alice".into()),
        }
    }

    #[tokio::test]
    async fn signed_out_profile_reads_as_none() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(read(&mut conn).await.unwrap(), None);
    }

    #[tokio::test]
    async fn opaque_tokens_round_trip() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        write(&mut conn, &pair()).await.unwrap();
        assert_eq!(read(&mut conn).await.unwrap(), Some(pair()));
    }

    #[tokio::test]
    async fn rotation_replaces_both_halves() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        write(&mut conn, &pair()).await.unwrap();

        let rotated = TokenPair {
            access_token: "wfa_new".into(),
            refresh_token: "wfr_new".into(),
            expires_at: Some(2_000_000),
            username: Some("alice".into()),
        };
        write(&mut conn, &rotated).await.unwrap();

        let stored = read(&mut conn).await.unwrap().unwrap();
        assert_eq!(stored, rotated, "a spent refresh token must not survive");
    }

    #[tokio::test]
    async fn a_credential_without_a_refresh_token_reads_as_signed_out() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        // Shape a v1 row left behind by an older build.
        sqlx::query(
            "INSERT INTO auth_credential
                (provider, token_encrypted, refresh_token_encrypted, created_at, updated_at)
             VALUES (?, ?, NULL, 0, 0)",
        )
        .bind(PROVIDER)
        .bind("header.payload.signature".as_bytes())
        .execute(&mut *conn)
        .await
        .unwrap();

        assert_eq!(read(&mut conn).await.unwrap(), None);
    }

    #[tokio::test]
    async fn empty_halves_are_refused_rather_than_stored() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut empty = pair();
        empty.refresh_token = "   ".into();
        assert!(write(&mut conn, &empty).await.is_err());
        assert_eq!(read(&mut conn).await.unwrap(), None);
    }

    #[test]
    fn refresh_margin_fires_before_the_token_actually_lapses() {
        let pair = TokenPair {
            expires_at: Some(10_000),
            ..pair()
        };
        assert!(!pair.needs_refresh(0, 1_000));
        // Still valid for half a second, but not for the round-trip.
        assert!(pair.needs_refresh(9_500, 1_000));
        assert!(pair.needs_refresh(10_001, 0));
    }

    #[test]
    fn a_credential_without_an_expiry_is_never_pre_emptively_refreshed() {
        let pair = TokenPair {
            expires_at: None,
            ..pair()
        };
        assert!(!pair.needs_refresh(i64::MAX / 2, 1_000));
    }

    #[test]
    fn a_non_positive_expires_in_yields_no_deadline() {
        assert_eq!(deadline_from_expires_in(0), None);
        assert_eq!(deadline_from_expires_in(-1), None);
        assert!(deadline_from_expires_in(900).is_some());
    }

    #[test]
    fn an_absurd_expires_in_does_not_overflow_into_the_past() {
        // A hostile or buggy server answering i64::MAX seconds must not
        // wrap the deadline around into a value that reads as expired.
        assert_eq!(deadline_from_expires_in(i64::MAX), None);
    }
}
