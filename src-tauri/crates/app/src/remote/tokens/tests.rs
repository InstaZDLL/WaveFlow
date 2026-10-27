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
