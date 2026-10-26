use super::{
    persist_setting, queue_is_album_ordered, read_bool_setting, read_setting, SettingType,
};
use sqlx::SqlitePool;

/// The real profile schema. `queue_is_album_ordered` reads
/// `playlist.smart_rules` through a query nothing verifies at
/// compile time — a wrong column name would compile, run, and
/// answer `false` for every Daily Mix there will ever be.
async fn migrated_pool() -> SqlitePool {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let options = SqliteConnectOptions::from_str(":memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO playlist (id, name, is_smart, smart_rules, created_at, updated_at)
                  VALUES (1, 'Daily Mix 1', 1,
                          '{\"kind\":\"daily_mix\",\"slot\":1,\"album_mode\":true}', 0, 0),
                         (2, 'Daily Mix 2', 1,
                          '{\"kind\":\"daily_mix\",\"slot\":2,\"album_mode\":false}', 0, 0),
                         (3, 'Daily Mix 3', 1,
                          '{\"kind\":\"daily_mix\",\"slot\":3}', 0, 0),
                         (4, 'On Repeat', 1, '{\"kind\":\"on_repeat\"}', 0, 0),
                         (5, 'Mine', 0, NULL, 0, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// A mix that was built out of records says so weeks later, and one
/// that fell back to tracks says that instead.
#[tokio::test]
async fn a_daily_mix_is_read_back_out_of_its_own_rule() {
    let pool = migrated_pool().await;

    assert!(queue_is_album_ordered(&pool, "playlist", Some(1), None).await);
    assert!(!queue_is_album_ordered(&pool, "playlist", Some(2), None).await);
    assert!(
        !queue_is_album_ordered(&pool, "playlist", Some(3), None).await,
        "a rule written before the flag existed describes a mix of tracks"
    );
    assert!(!queue_is_album_ordered(&pool, "playlist", Some(4), None).await);
    assert!(!queue_is_album_ordered(&pool, "playlist", Some(5), None).await);
    assert!(
        !queue_is_album_ordered(&pool, "playlist", Some(404), None).await,
        "a playlist that is not there is not a session of records"
    );
}

/// Mood Radio knows as it answers, and nothing else may be asked to
/// guess: an album, an artist page, the library are what their own
/// `source_type` says.
#[tokio::test]
async fn a_declared_answer_is_taken_and_nothing_else_is_invented() {
    let pool = migrated_pool().await;

    assert!(queue_is_album_ordered(&pool, "radio", None, Some(true)).await);
    assert!(
        !queue_is_album_ordered(&pool, "radio", None, Some(false)).await,
        "a mood that fell back to tracks produced tracks"
    );
    assert!(
        !queue_is_album_ordered(&pool, "radio", None, None).await,
        "and one that declares nothing is not assumed to be records"
    );
    for source in ["album", "artist", "library", "liked", "manual"] {
        assert!(!queue_is_album_ordered(&pool, source, Some(1), None).await);
    }
}
/// The decimals the engine persists (the two ReplayGain gains, the
/// playback speed) go through the table's CHECK and read back as the
/// restore parses them. They were written as `'float'`, which the
/// CHECK refuses — the first assertion is that refusal, kept so the
/// reason for `SettingType::Text` stays on record.
#[tokio::test]
async fn a_decimal_setting_is_stored_and_read_back() {
    let pool = migrated_pool().await;

    let refused = sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.playback_speed', '1.25', 'float', 0)",
    )
    .execute(&pool)
    .await;
    assert!(refused.is_err(), "the CHECK accepts no 'float' marker");

    for (key, value) in [
        ("audio.replaygain_preamp", -3.5_f32),
        ("audio.replaygain_fallback", 2.0),
        ("audio.playback_speed", 1.25),
    ] {
        persist_setting(&pool, key, value.to_string(), SettingType::Text)
            .await
            .unwrap();
        let read = read_setting(&pool, key)
            .await
            .and_then(|v| v.parse::<f32>().ok());
        assert_eq!(read, Some(value), "{key} did not survive the round trip");
    }

    // A second write replaces the value rather than adding a row.
    persist_setting(&pool, "audio.playback_speed", "0.75", SettingType::Text)
        .await
        .unwrap();
    assert_eq!(
        read_setting(&pool, "audio.playback_speed").await.as_deref(),
        Some("0.75")
    );
}

/// A write that fails is reported, not swallowed: the commands return
/// this error before touching the engine, and the control rolls back.
#[tokio::test]
async fn a_failed_write_is_reported() {
    let pool = migrated_pool().await;
    pool.close().await;
    let result = persist_setting(&pool, "audio.mono", "true", SettingType::Bool).await;
    assert!(result.is_err(), "a write to a closed database succeeded");
}

/// Every marker `persist_setting` can write is one the table accepts.
#[tokio::test]
async fn every_setting_type_passes_the_check() {
    let pool = migrated_pool().await;
    for (key, value, value_type) in [
        ("test.bool", "true", SettingType::Bool),
        ("test.int", "256", SettingType::Int),
        ("test.text", "auto", SettingType::Text),
        ("test.json", "[0,0,0]", SettingType::Json),
    ] {
        persist_setting(&pool, key, value, value_type)
            .await
            .unwrap();
        assert_eq!(read_setting(&pool, key).await.as_deref(), Some(value));
        let marker: String =
            sqlx::query_scalar("SELECT value_type FROM profile_setting WHERE key = ?")
                .bind(key)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(marker, value_type.as_marker());
    }
}

/// A profile that never wrote a setting must read as the default,
/// not as whatever the previous profile left behind (#698). The
/// same holds for a row that says something else entirely.
#[tokio::test]
async fn an_unset_toggle_reads_as_its_default() {
    let pool = migrated_pool().await;

    assert!(!read_bool_setting(&pool, "ui.visualizer", false).await);
    assert!(
        read_bool_setting(&pool, "audio.gapless", true).await,
        "a setting whose default is on stays on while nothing says otherwise"
    );

    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('ui.visualizer', 'true', 'bool', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(read_bool_setting(&pool, "ui.visualizer", false).await);

    sqlx::query("UPDATE profile_setting SET value = '1' WHERE key = 'ui.visualizer'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        read_bool_setting(&pool, "ui.visualizer", false).await,
        "the older 1 / 0 spelling still reads as a boolean"
    );

    sqlx::query("UPDATE profile_setting SET value = 'yes' WHERE key = 'ui.visualizer'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !read_bool_setting(&pool, "ui.visualizer", false).await,
        "and a value that is not a boolean at all reads as the default"
    );
    assert!(
        read_bool_setting(&pool, "ui.visualizer", true).await,
        "which means the default, whichever way it points"
    );
}
