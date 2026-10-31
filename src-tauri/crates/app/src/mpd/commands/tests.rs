use super::*;

#[test]
fn repeat_maps_onto_mpds_two_flags() {
    assert_eq!(repeat_flags(RepeatMode::Off), (false, false));
    assert_eq!(repeat_flags(RepeatMode::All), (true, false));
    // "Repeat one" is `repeat 1` + `single 1` in MPD.
    assert_eq!(repeat_flags(RepeatMode::One), (true, true));
}

#[test]
fn toggling_repeat_preserves_single() {
    // The client only touched the repeat bit, so a queue already in
    // "repeat one" must not silently downgrade to "repeat all".
    assert_eq!(repeat_from_mpd(RepeatMode::One, true), RepeatMode::One);
    assert_eq!(repeat_from_mpd(RepeatMode::Off, true), RepeatMode::All);
    assert_eq!(repeat_from_mpd(RepeatMode::One, false), RepeatMode::Off);
}

#[test]
fn toggling_single_preserves_repeat() {
    assert_eq!(single_from_mpd(RepeatMode::All, true), RepeatMode::One);
    // Turning single off from "repeat one" leaves repeat on, which
    // is what MPD's own flags do.
    assert_eq!(single_from_mpd(RepeatMode::One, false), RepeatMode::All);
    assert_eq!(single_from_mpd(RepeatMode::Off, false), RepeatMode::Off);
}

#[test]
fn single_oneshot_is_rejected_not_persisted_as_durable() {
    // `single 0` / `single 1` resolve to a durable repeat mode…
    assert_eq!(
        single_action(SingleMode::On, RepeatMode::All),
        Some(RepeatMode::One)
    );
    assert_eq!(
        single_action(SingleMode::Off, RepeatMode::One),
        Some(RepeatMode::All)
    );
    // …but `single oneshot` has no durable equivalent, so it is rejected
    // (None → the handler ACKs "unsupported") rather than silently stored
    // as a permanent `single 1`.
    assert_eq!(single_action(SingleMode::OneShot, RepeatMode::All), None);
    assert_eq!(single_action(SingleMode::OneShot, RepeatMode::Off), None);
}

#[test]
fn repeat_flags_round_trip_through_both_setters() {
    for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
        let (repeat, single) = repeat_flags(mode);
        let after_repeat = repeat_from_mpd(mode, repeat);
        let after_both = single_from_mpd(after_repeat, single);
        assert_eq!(after_both, mode, "{mode:?} did not survive a round trip");
    }
}

#[test]
fn seek_targets_keep_their_fractional_seconds() {
    // The bug this guards: `seconds as u64 * 1000` floors first, so
    // a half-second seek target collapsed to the whole second.
    assert_eq!(seconds_to_ms(12.5), 12_500);
    assert_eq!(seconds_to_ms(0.25), 250);
    assert_eq!(seconds_to_ms(0.0), 0);
    // MPD never sends a negative absolute target, but clamping
    // beats wrapping to u64::MAX if one arrives.
    assert_eq!(seconds_to_ms(-5.0), 0);
}

#[test]
fn advertised_commands_are_sorted_and_unique() {
    // `commands` output feeds client capability detection; a
    // duplicate would be harmless but signals a bad merge.
    let mut sorted = SUPPORTED_COMMANDS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.as_slice(), SUPPORTED_COMMANDS);
}

/// An autoplay top-up holds the publish lock from reading the queue's end
/// to appending its picks; a `clear` sent meanwhile waits for it, so it
/// empties the picks too instead of being followed by them.
#[tokio::test]
async fn clear_waits_for_a_top_up_under_the_publish_lock() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
    use std::str::FromStr;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let opts = SqliteConnectOptions::from_str(&format!(
        "sqlite://{}",
        dir.path().join("profile.db").display()
    ))
    .unwrap()
    .create_if_missing(true)
    .foreign_keys(true);
    let pool = SqlitePool::connect_with(opts).await.unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
         INSERT INTO artist (id, name, canonical_name) VALUES (1, 'a', 'a');",
    )
    .execute(&pool)
    .await
    .unwrap();
    for id in 1..=4_i64 {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, primary_artist, duration_ms, added_at)
             VALUES (?, 1, ?, ?, 1, 0, 't', 1, 1000, 0)",
        )
        .bind(id)
        .bind(format!("/l/{id}.flac"))
        .bind(format!("h{id}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    queue::fill_queue(&pool, "album", None, &[1, 2, 3], 2, false)
        .await
        .unwrap();

    let publish = Arc::new(tokio::sync::Mutex::new(()));
    // The top-up has read the queue's end and is drawing its picks.
    let top_up = publish.clone().lock_owned().await;
    let clear = tokio::spawn({
        let (publish, pool) = (publish.clone(), pool.clone());
        async move { clear_queue(publish.lock_owned(), &pool).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(queue::queue_length(&pool).await.unwrap(), 3);
    // Its picks are appended, then it lets go.
    queue::append(&pool, &[4], "radio", None).await.unwrap();
    drop(top_up);
    clear.await.unwrap().unwrap();
    assert_eq!(queue::queue_length(&pool).await.unwrap(), 0);
}
