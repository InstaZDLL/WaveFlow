//! A setting command writes into the profile its control was acted on, or
//! nowhere (issue #996). Driven through AutoMix's command body, on a
//! profile slot of the test's own: an `AppState` cannot be built here.

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use super::*;
use crate::{
    audio::state::SharedPlayback,
    error::AppError,
    state::{while_profile_is, ActiveProfile},
};

/// A profile database with the repo's migrations, `foreign_keys` on as on
/// every profile pool.
async fn profile_db(dir: &std::path::Path, name: &str) -> SqlitePool {
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", dir.join(name).display()))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(opts).await.unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

async fn automix_row(pool: &SqlitePool) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = 'audio.automix'")
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_toggle_lands_in_the_profile_it_was_flipped_in() {
    let dir = tempfile::tempdir().unwrap();
    let a = profile_db(dir.path(), "a.db").await;
    let slot = RwLock::new(Some(ActiveProfile::for_test(1, a.clone())));
    let shared = SharedPlayback::new();

    set_automix(&slot, &shared, true, Some(1)).await.unwrap();

    assert_eq!(automix_row(&a).await.as_deref(), Some("true"));
    assert!(shared.automix.is_enabled());
}

#[tokio::test]
async fn a_toggle_for_a_profile_since_left_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let a = profile_db(dir.path(), "a.db").await;
    let b = profile_db(dir.path(), "b.db").await;
    let slot = RwLock::new(Some(ActiveProfile::for_test(1, a.clone())));
    let shared = SharedPlayback::new();

    // Flipped in profile 1; the switch to profile 2 lands before the
    // command reaches the pool.
    *slot.write().await = Some(ActiveProfile::for_test(2, b.clone()));
    let err = set_automix(&slot, &shared, true, Some(1))
        .await
        .unwrap_err();

    assert!(
        matches!(
            err,
            AppError::ProfileChanged {
                expected: 1,
                active: Some(2)
            }
        ),
        "unexpected error: {err}",
    );
    // Nothing written anywhere, and the engine keeps what profile 2's
    // restore put there.
    assert_eq!(automix_row(&b).await, None);
    assert_eq!(automix_row(&a).await, None);
    assert!(!shared.automix.is_enabled());
}

#[tokio::test]
async fn a_toggle_for_a_profile_since_closed_is_refused() {
    let slot = RwLock::new(None);
    let shared = SharedPlayback::new();

    let err = set_automix(&slot, &shared, true, Some(1))
        .await
        .unwrap_err();

    assert!(
        matches!(
            err,
            AppError::ProfileChanged {
                expected: 1,
                active: None
            }
        ),
        "unexpected error: {err}",
    );
    assert!(!shared.automix.is_enabled());
}

#[tokio::test]
async fn no_profile_in_mind_and_none_active_applies_without_saving() {
    // The contract the setters always had: a caller with no profile in
    // mind, with none open, still changes what plays.
    let slot = RwLock::new(None);
    let shared = SharedPlayback::new();

    set_automix(&slot, &shared, true, None).await.unwrap();

    assert!(shared.automix.is_enabled());
}

#[tokio::test]
async fn a_switch_after_the_save_keeps_the_engine_on_the_next_profile() {
    // The command already saved into profile 1 when the switch lands: the
    // engine step is skipped, so it cannot overwrite profile 2's restore.
    let dir = tempfile::tempdir().unwrap();
    let a = profile_db(dir.path(), "a.db").await;
    let b = profile_db(dir.path(), "b.db").await;
    let slot = RwLock::new(Some(ActiveProfile::for_test(1, a)));

    *slot.write().await = Some(ActiveProfile::for_test(2, b));

    assert_eq!(while_profile_is(&slot, Some(1), || ()).await, None);
    assert_eq!(while_profile_is(&slot, Some(2), || 7).await, Some(7));
    assert_eq!(while_profile_is(&slot, None, || ()).await, None);
}
