use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::audio::engine::{LoadIntent, TestEngine};

const OLDER: LoadIntent = LoadIntent::from_raw(5);
const STATION: LoadIntent = LoadIntent::from_raw(6);
const NEWER: LoadIntent = LoadIntent::from_raw(7);

/// A library track picked while this station waited for the lock. Its queue, its
/// label and any session it started are what stand: a station publishing here
/// would clear them and name itself to the OS overlay, for a load the decoder
/// then drops.
#[tokio::test]
async fn a_station_that_lost_its_turn_publishes_nothing() {
    let engine = TestEngine::new();
    engine.claims(NEWER);
    let published = AtomicUsize::new(0);
    let played = publish_on_turn(&engine, STATION, || {
        published.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await;
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(published.load(Ordering::SeqCst), 0);
}

/// A track picked before the station and still reading its queue is the older
/// choice. It has to find out before it relabels the player: the decoder drops
/// its load, and nothing would correct the label.
#[tokio::test]
async fn a_station_overtakes_a_pick_still_preparing() {
    let engine = TestEngine::new();
    let played = publish_on_turn(&engine, STATION, || Ok(())).await;
    assert!(matches!(played, Ok(true)), "{played:?}");
    assert!(
        !engine.claim_dispatch(OLDER),
        "the older pick still has a turn"
    );
}

/// A local producer claims and sends as one move. A station cutting in would
/// clear the session and send its load in the middle of it.
#[tokio::test]
async fn a_station_waits_for_a_publisher_already_at_work() {
    let engine = TestEngine::new();
    let published = AtomicUsize::new(0);
    let guard = engine.publish.lock().await;
    let publishing = async {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            published.load(Ordering::SeqCst),
            0,
            "published under a publisher's feet"
        );
        engine.claims(NEWER);
        drop(guard);
    };
    let station = publish_on_turn(&engine, STATION, || {
        published.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let (played, ()) = tokio::join!(station, publishing);
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(published.load(Ordering::SeqCst), 0);
}

/// A failed send is the caller's to report, as for any load.
#[tokio::test]
async fn a_failed_publish_is_reported() {
    let engine = TestEngine::new();
    let played = publish_on_turn(&engine, STATION, || {
        Err(AppError::Other("the decoder is gone".into()))
    })
    .await;
    assert!(played.is_err());
}
