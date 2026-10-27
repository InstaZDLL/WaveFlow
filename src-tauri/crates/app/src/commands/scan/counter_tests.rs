use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

/// A backfill's attempt counter has to survive `profile_setting`'s
/// `value_type` CHECK. It wrote `'number'`, which the CHECK refuses: no
/// attempt was ever recorded, the retry cap never reached, and a folder
/// holding one unreadable file was re-read in full on every scan.
#[tokio::test]
async fn an_attempt_counter_is_written_and_read_back() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::from_str(":memory:").unwrap())
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    let key = "scan.genre_backfill_attempts.1";
    assert_eq!(read_counter(&pool, key, 1).await, 0);
    write_counter(&pool, key, 1)
        .await
        .expect("the CHECK accepts it");
    write_counter(&pool, key, 2).await.unwrap();
    assert_eq!(read_counter(&pool, key, 1).await, 2);
}
