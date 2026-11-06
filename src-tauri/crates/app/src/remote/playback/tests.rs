use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::audio::engine::TestEngine as Engine;
use crate::error::AppError;

const STEP: LoadIntent = LoadIntent::from_raw(5);
const NEWER: LoadIntent = LoadIntent::from_raw(6);

fn entry(id: &str) -> RemoteEntry {
    RemoteEntry {
        id: id.into(),
        title: None,
        artist: None,
        artist_id: None,
        artwork_hash: None,
        duration_ms: None,
    }
}

fn queue(ids: &[&str]) -> RemoteQueue {
    RemoteQueue {
        entries: ids.iter().map(|id| entry(id)).collect(),
        index: 0,
    }
}

fn session(ids: &[&str]) -> RemotePlayback {
    let session = RemotePlayback::default();
    session.set(queue(ids));
    session
}

fn current(session: &RemotePlayback) -> Option<String> {
    session.current().map(|entry| entry.id)
}

fn refused() -> AppError {
    AppError::Other("the ticket was refused".into())
}

async fn step(
    engine: &Engine,
    session: &RemotePlayback,
    repeat: RepeatMode,
    play: impl std::future::Future<Output = AppResult<()>>,
) -> AppResult<bool> {
    step_and_play(engine, session, Direction::Next, repeat, STEP, play).await
}

// A step that fails.

/// The case that was reported: the ticket request hangs, the person picks a
/// library track, the request fails. What they picked must keep playing.
#[tokio::test]
async fn a_failed_step_leaves_a_newer_pick_playing() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let played = step(&engine, &session, RepeatMode::Off, async {
        engine.claims(NEWER);
        // What `emit_track_changed` does for a library track.
        session.clear();
        Err(refused())
    })
    .await;
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(engine.stops(), 0);
}

/// A remote queue started meanwhile is someone else's session: it is neither
/// stopped nor dropped.
#[tokio::test]
async fn a_failed_step_leaves_a_session_started_meanwhile() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let played = step(&engine, &session, RepeatMode::Off, async {
        engine.claims(NEWER);
        session.set(queue(&["other"]));
        Err(refused())
    })
    .await;
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(engine.stops(), 0);
    assert_eq!(current(&session).as_deref(), Some("other"));
}

/// A profile switch or a crossfade drops the session without claiming: gone is enough
/// to know this failure is nobody's business.
#[tokio::test]
async fn a_failed_step_stops_nothing_once_the_session_is_gone() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let played = step(&engine, &session, RepeatMode::Off, async {
        session.clear();
        Err(refused())
    })
    .await;
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(engine.stops(), 0);
}

/// Nothing overtook it: no phantom cursor is left for the next action.
#[tokio::test]
async fn a_failed_step_still_its_own_stops_and_drops_the_session() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let played = step(&engine, &session, RepeatMode::Off, async { Err(refused()) }).await;
    assert!(played.is_err());
    assert_eq!(engine.stops(), 1);
    assert!(!session.is_active());
}

/// A library track publishes under the lock: its claim and its load are one
/// move. Deciding outside the lock could read "not overtaken", let that move
/// happen, and only then send the `Stop`, which would land after its load.
#[tokio::test]
async fn giving_up_waits_for_a_publisher_already_at_work() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let (publishing, publisher) = tokio::sync::oneshot::channel();
    let stepping = step(&engine, &session, RepeatMode::Off, async {
        let _ = publishing.send(engine.publish.lock().await);
        Err(refused())
    });
    let publishes = async {
        let guard = publisher.await.unwrap();
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(engine.stops(), 0, "stopped under a publisher's feet");
        engine.claims(NEWER);
        session.clear();
        drop(guard);
    };
    let (played, ()) = tokio::join!(stepping, publishes);
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(engine.stops(), 0);
}

// A step that does not get its turn.

#[tokio::test]
async fn a_step_overtaken_before_its_turn_moves_nothing() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    engine.claims(NEWER);
    let asked = AtomicBool::new(false);
    let played = step(&engine, &session, RepeatMode::Off, async {
        asked.store(true, Ordering::SeqCst);
        Ok(())
    })
    .await;
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(current(&session).as_deref(), Some("a"));
    assert!(!asked.load(Ordering::SeqCst), "the server was asked");
}

/// Running off the end stops the player, so an overtaken step must not get
/// there: the newer pick would go silent.
#[tokio::test]
async fn an_overtaken_step_at_the_end_of_the_queue_stops_nothing() {
    let engine = Engine::new();
    let session = session(&["a"]);
    engine.claims(NEWER);
    let played = step(&engine, &session, RepeatMode::Off, async { Ok(()) }).await;
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(engine.stops(), 0);
    assert_eq!(current(&session).as_deref(), Some("a"));
}

/// The callers test `is_active` before reading the repeat mode from the
/// database. A session dropped in that gap is not a queue that ran out, and the
/// load of whoever dropped it may still be on its way to the decoder, which
/// would drop it for a claim made here.
#[tokio::test]
async fn a_step_finding_no_session_stops_and_claims_nothing() {
    let engine = Engine::new();
    let session = RemotePlayback::default();
    let played = step(&engine, &session, RepeatMode::Off, async { Ok(()) }).await;
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(engine.stops(), 0);
    assert_eq!(engine.newest_claim(), 0);
}

// What a step still does.

#[tokio::test]
async fn running_off_the_end_stops_the_player() {
    let engine = Engine::new();
    let session = session(&["a"]);
    let played = step(&engine, &session, RepeatMode::Off, async { Ok(()) }).await;
    assert!(matches!(played, Ok(false)), "{played:?}");
    assert_eq!(engine.stops(), 1);
    assert!(!session.is_active());
}

#[tokio::test]
async fn a_step_moves_the_cursor_and_plays() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let played = step(&engine, &session, RepeatMode::Off, async {
        assert_eq!(current(&session).as_deref(), Some("b"));
        Ok(())
    })
    .await;
    assert!(matches!(played, Ok(true)), "{played:?}");
    assert_eq!(engine.stops(), 0);
}

/// The request is a network round-trip: under the lock it would park every other
/// surface for its whole length.
#[tokio::test]
async fn the_request_runs_outside_the_publish_lock() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let played = step(&engine, &session, RepeatMode::Off, async {
        assert!(engine.publish.try_lock().is_ok(), "held across the request");
        Ok(())
    })
    .await;
    assert!(matches!(played, Ok(true)), "{played:?}");
}

// A jump, the queue panel's click.

#[tokio::test]
async fn a_jump_overtaken_before_its_turn_moves_nothing() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    engine.claims(NEWER);
    let asked = AtomicBool::new(false);
    let played = jump_and_play(&engine, &session, 1, STEP, async {
        asked.store(true, Ordering::SeqCst);
        Ok(())
    })
    .await;
    assert!(played.is_ok());
    assert_eq!(current(&session).as_deref(), Some("a"));
    assert!(!asked.load(Ordering::SeqCst), "the server was asked");
}

/// A jump is a newer pick for a step still waiting on its ticket. It has to say
/// so before its own request, or that step, failing, would stop the player and
/// drop the queue the jump is about to play from.
#[tokio::test]
async fn a_jump_claims_its_turn_before_its_request() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let played = jump_and_play(&engine, &session, 1, NEWER, async {
        assert!(Turns::load_intent_superseded(&engine, STEP));
        Ok(())
    })
    .await;
    assert!(played.is_ok());
    assert_eq!(current(&session).as_deref(), Some("b"));
}

#[tokio::test]
async fn a_failed_jump_leaves_a_newer_pick_playing() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let played = jump_and_play(&engine, &session, 1, STEP, async {
        engine.claims(NEWER);
        session.clear();
        Err(refused())
    })
    .await;
    assert!(played.is_ok(), "{played:?}");
    assert_eq!(engine.stops(), 0);
}

/// Offline with a downloaded album, a click on a row that was not downloaded:
/// what plays goes on, and the next step starts from it.
#[tokio::test]
async fn a_failed_jump_still_its_own_puts_the_cursor_back() {
    let engine = Engine::new();
    let session = session(&["a", "b"]);
    let played = jump_and_play(&engine, &session, 1, STEP, async { Err(refused()) }).await;
    assert!(played.is_err());
    assert_eq!(engine.stops(), 0);
    assert_eq!(current(&session).as_deref(), Some("a"));
}

/// The cursor is the newer move's by then.
#[tokio::test]
async fn a_failed_jump_leaves_the_cursor_of_a_newer_move() {
    let engine = Engine::new();
    let session = session(&["a", "b", "c"]);
    let played = jump_and_play(&engine, &session, 1, STEP, async {
        engine.claims(NEWER);
        session.seek_to(2);
        Err(refused())
    })
    .await;
    assert!(played.is_ok(), "{played:?}");
    assert_eq!(current(&session).as_deref(), Some("c"));
}

#[tokio::test]
async fn a_jump_finding_no_session_claims_nothing() {
    let engine = Engine::new();
    let session = RemotePlayback::default();
    let played = jump_and_play(&engine, &session, 1, STEP, async { Ok(()) }).await;
    assert!(played.is_ok());
    assert_eq!(engine.newest_claim(), 0);
}

// A start that did not reach the engine.

fn nothing_before() -> SessionsBefore {
    SessionsBefore {
        remote: None,
        files: None,
        episodes: None,
    }
}

fn no_files() -> ExternalPlayback {
    ExternalPlayback::default()
}

fn no_episodes() -> PodcastPlayback {
    PodcastPlayback::default()
}

/// Left installed, a queue nothing plays from takes over Next: the next press
/// asks the same server again and, when that fails, stops the track the person
/// was still listening to.
#[test]
fn a_failed_start_leaves_no_session_behind() {
    let session = RemotePlayback::default();
    let revision = session.set(queue(&["a", "b"]));
    let settled = settle_start(
        &session,
        &no_files(),
        &no_episodes(),
        revision,
        false,
        nothing_before(),
        Err(refused()),
    );
    assert!(settled.is_err());
    assert!(!session.is_active());
}

/// An album still playing keeps its queue when the next one could not start.
#[test]
fn a_failed_start_puts_back_the_remote_queue_it_replaced() {
    let session = session(&["a", "b"]);
    session.seek_to(1);
    let before = SessionsBefore {
        remote: session.queue(),
        files: None,
        episodes: None,
    };
    let revision = session.set(queue(&["c"]));
    let settled = settle_start(
        &session,
        &no_files(),
        &no_episodes(),
        revision,
        false,
        before,
        Err(refused()),
    );
    assert!(settled.is_err());
    assert_eq!(current(&session).as_deref(), Some("b"));
}

#[test]
fn a_failed_start_puts_back_the_files_it_replaced() {
    let session = RemotePlayback::default();
    let files = no_files();
    let before = SessionsBefore {
        remote: None,
        files: Some(DetachedQueue {
            entries: vec![ExternalEntry {
                path: "/music/a.flac".into(),
            }],
            index: 0,
        }),
        episodes: None,
    };
    let revision = session.set(queue(&["c"]));
    let settled = settle_start(
        &session,
        &files,
        &no_episodes(),
        revision,
        false,
        before,
        Err(refused()),
    );
    assert!(settled.is_err());
    assert!(!session.is_active());
    assert!(files.is_active());
}

/// An episode still playing keeps its queue, and its position is saved again.
#[test]
fn a_failed_start_puts_back_the_episodes_it_replaced() {
    let session = RemotePlayback::default();
    let episodes = no_episodes();
    let before = SessionsBefore {
        remote: None,
        files: None,
        episodes: Some(DetachedQueue {
            entries: vec![EpisodeEntry {
                home: crate::podcast::playback::EpisodeHome::Local {
                    episode_id: 7,
                    channel_id: 1,
                },
                profile_id: 1,
                title: "Episode".into(),
                channel: "Show".into(),
                duration_ms: None,
            }],
            index: 0,
        }),
    };
    let revision = session.set(queue(&["c"]));
    let settled = settle_start(
        &session,
        &no_files(),
        &episodes,
        revision,
        false,
        before,
        Err(refused()),
    );
    assert!(settled.is_err());
    assert!(!session.is_active());
    assert_eq!(
        episodes.current().map(|entry| entry.title).as_deref(),
        Some("Episode")
    );
}

/// A profile switch during the ticket clears every session and claims nothing.
/// Putting the files back would give the next profile the last one's queue.
#[test]
fn a_failed_start_puts_nothing_back_after_a_profile_switch() {
    let session = RemotePlayback::default();
    let files = no_files();
    let before = SessionsBefore {
        remote: Some(queue(&["old"])),
        files: Some(DetachedQueue {
            entries: vec![ExternalEntry {
                path: "/music/a.flac".into(),
            }],
            index: 0,
        }),
        episodes: None,
    };
    let revision = session.set(queue(&["c"]));
    // What `activate_profile` does to the three sessions.
    session.clear();
    files.clear();
    let settled = settle_start(
        &session,
        &files,
        &no_episodes(),
        revision,
        false,
        before,
        Err(refused()),
    );
    assert!(settled.is_err());
    assert!(!session.is_active());
    assert!(!files.is_active());
}

#[test]
fn a_failed_start_keeps_the_queue_someone_has_since_moved_in() {
    let session = RemotePlayback::default();
    let revision = session.set(queue(&["a", "b"]));
    session.seek_to(1);
    let settled = settle_start(
        &session,
        &no_files(),
        &no_episodes(),
        revision,
        false,
        nothing_before(),
        Err(refused()),
    );
    assert!(settled.is_err());
    assert_eq!(current(&session).as_deref(), Some("b"));
}

/// A newer pick owns the moment: what the start replaced does not come back.
#[test]
fn an_overtaken_start_rolls_its_install_back_and_restores_nothing() {
    let session = RemotePlayback::default();
    let before = SessionsBefore {
        remote: Some(queue(&["old"])),
        files: None,
        episodes: None,
    };
    let revision = session.set(queue(&["a", "b"]));
    let settled = settle_start(
        &session,
        &no_files(),
        &no_episodes(),
        revision,
        true,
        before,
        Err(refused()),
    );
    assert!(settled.is_err());
    assert!(!session.is_active());
}

#[test]
fn a_start_that_worked_stays_installed() {
    let session = RemotePlayback::default();
    let before = SessionsBefore {
        remote: Some(queue(&["old"])),
        files: None,
        episodes: None,
    };
    let revision = session.set(queue(&["a", "b"]));
    let settled = settle_start(
        &session,
        &no_files(),
        &no_episodes(),
        revision,
        false,
        before,
        Ok(()),
    );
    assert!(settled.is_ok());
    assert_eq!(current(&session).as_deref(), Some("a"));
}
