use super::*;

#[test]
fn a_folder_expands_to_its_audio_files_in_name_order() {
    let dir = tempfile::tempdir().unwrap();
    let disc = dir.path().join("CD2");
    std::fs::create_dir(&disc).unwrap();
    for name in ["b.flac", "a.mp3", "notes.txt", "cover.jpg"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    std::fs::write(disc.join("c.flac"), b"x").unwrap();
    let ours = dir.path().join(".waveflow");
    std::fs::create_dir(&ours).unwrap();
    std::fs::write(ours.join("clip.mp4"), b"x").unwrap();

    let files = expand(&[dir.path().to_path_buf()]);

    let names: Vec<_> = files
        .iter()
        .map(|p| {
            p.strip_prefix(dir.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(names, ["CD2/c.flac", "a.mp3", "b.flac"]);
}

/// A link to an audio file plays as the file. Unix only: creating a
/// link on Windows needs a privilege the CI runner may not have.
#[cfg(unix)]
#[test]
fn a_link_to_an_audio_file_counts_as_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("elsewhere.flac");
    std::fs::write(&target, b"x").unwrap();
    let folder = dir.path().join("album");
    std::fs::create_dir(&folder).unwrap();
    std::os::unix::fs::symlink(&target, folder.join("linked.flac")).unwrap();

    assert_eq!(
        expand(std::slice::from_ref(&folder)),
        [folder.join("linked.flac")]
    );
}

#[test]
fn files_keep_their_order_and_other_files_are_passed_over() {
    let dir = tempfile::tempdir().unwrap();
    let (b, a, txt) = (
        dir.path().join("b.flac"),
        dir.path().join("a.flac"),
        dir.path().join("readme.txt"),
    );
    for path in [&b, &a, &txt] {
        std::fs::write(path, b"x").unwrap();
    }
    let missing = dir.path().join("gone.flac");

    assert_eq!(expand(&[b.clone(), txt, missing, a.clone()]), [b, a]);
}

#[test]
fn a_handoff_keeps_its_place_when_files_are_gone() {
    let dir = tempfile::tempdir().unwrap();
    let [a, b, c, d] = ["a.flac", "b.flac", "c.flac", "d.flac"].map(|n| dir.path().join(n));
    for path in [&a, &c, &d] {
        std::fs::write(path, b"x").unwrap();
    }
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, b"x").unwrap();
    let queue = [a.clone(), b.clone(), notes, c.clone(), d.clone()];

    // The current file is still there: it keeps its place.
    assert_eq!(
        handed_over_queue(&queue, 3),
        Some((vec![a.clone(), c.clone(), d.clone()], 1))
    );
    // It is gone: the next one that is there.
    assert_eq!(
        handed_over_queue(&queue, 1),
        Some((vec![a.clone(), c.clone(), d.clone()], 1))
    );
    // Out of range: the last one.
    assert_eq!(handed_over_queue(&queue, 9), Some((vec![a, c, d], 2)));
    assert_eq!(handed_over_queue(&[b], 0), None);
}

/// A length of 0 is what the engine takes for a live stream, and lofty answers
/// it for a WavPack file whose first block states none, as a piped encode does.
#[test]
fn a_wavpack_file_that_states_no_length_is_measured_by_the_decoder() {
    use lofty::prelude::AudioFile;

    let dir = tempfile::tempdir().unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/lossless/s16_stereo.wv");
    let mut bytes = std::fs::read(&fixture).unwrap();
    bytes[12..16].fill(0xFF);
    let path = dir.path().join("unstated.wv");
    std::fs::write(&path, bytes).unwrap();
    let tagged = waveflow_core::scanner::extract::read_tagged_file(&path).unwrap();
    assert!(tagged.properties().duration().is_zero());

    let facts = read_facts(&path, dir.path());

    assert_eq!(
        facts.duration_ms,
        read_facts(&fixture, dir.path()).duration_ms
    );
    assert_ne!(facts.duration_ms, 0);
}

#[test]
fn a_file_without_tags_reads_as_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Track 07.flac");
    std::fs::write(&path, b"not really a flac").unwrap();

    let facts = read_facts(&path, dir.path());

    assert_eq!(facts.title, "Track 07");
    assert_eq!(facts.artist, None);
    assert_eq!(facts.cover, None);
}

// Moving through the queue, on the engine's real arbitration.

mod turns {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::audio::engine::TestEngine;
    use crate::queue::RepeatMode;

    const MOVE: LoadIntent = LoadIntent::from_raw(5);
    const NEWER: LoadIntent = LoadIntent::from_raw(6);

    fn session(names: &[&str]) -> ExternalPlayback {
        let session = ExternalPlayback::default();
        session.set(DetachedQueue {
            entries: names
                .iter()
                .map(|name| ExternalEntry { path: name.into() })
                .collect(),
            index: 0,
        });
        session
    }

    fn current(session: &ExternalPlayback) -> Option<String> {
        session
            .current()
            .map(|entry| entry.path.display().to_string())
    }

    fn unreadable() -> AppError {
        AppError::Other("the file could not be read".into())
    }

    async fn step(
        engine: &TestEngine,
        session: &ExternalPlayback,
        play: impl std::future::Future<Output = AppResult<()>>,
    ) -> AppResult<bool> {
        step_and_play(
            engine,
            session,
            Direction::Next,
            RepeatMode::Off,
            MOVE,
            play,
        )
        .await
    }

    /// `is_active` is read before the repeat mode, and the session can go in
    /// that gap. A library track doing that has its load on the way: a `Stop`
    /// here would land behind it, and a claim here would have the decoder drop it.
    #[tokio::test]
    async fn a_step_finding_no_session_stops_and_claims_nothing() {
        let engine = TestEngine::new();
        let session = ExternalPlayback::default();
        let played = step(&engine, &session, async { Ok(()) }).await;
        assert!(matches!(played, Ok(false)), "{played:?}");
        assert_eq!(engine.stops(), 0);
        assert_eq!(engine.newest_claim(), 0);
    }

    #[tokio::test]
    async fn a_jump_finding_no_session_claims_nothing() {
        let engine = TestEngine::new();
        let session = ExternalPlayback::default();
        let played = jump_and_play(&engine, &session, 1, MOVE, async { Ok(()) }).await;
        assert!(played.is_ok());
        assert_eq!(engine.stops(), 0);
        assert_eq!(engine.newest_claim(), 0);
    }

    /// A profile switch drops the session without the lock: what it left is
    /// nobody's to stop.
    #[tokio::test]
    async fn a_failed_step_stops_nothing_once_the_session_is_gone() {
        let engine = TestEngine::new();
        let session = session(&["a", "b"]);
        let played = step(&engine, &session, async {
            session.clear();
            Err(unreadable())
        })
        .await;
        assert!(matches!(played, Ok(false)), "{played:?}");
        assert_eq!(engine.stops(), 0);
    }

    #[tokio::test]
    async fn a_failed_jump_stops_nothing_once_the_session_is_gone() {
        let engine = TestEngine::new();
        let session = session(&["a", "b"]);
        let played = jump_and_play(&engine, &session, 1, MOVE, async {
            session.clear();
            Err(unreadable())
        })
        .await;
        assert!(played.is_ok(), "{played:?}");
        assert_eq!(engine.stops(), 0);
    }

    #[tokio::test]
    async fn an_overtaken_step_moves_nothing() {
        let engine = TestEngine::new();
        let session = session(&["a", "b"]);
        engine.claims(NEWER);
        let read = AtomicBool::new(false);
        let played = step(&engine, &session, async {
            read.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert!(matches!(played, Ok(false)), "{played:?}");
        assert_eq!(current(&session).as_deref(), Some("a"));
        assert!(!read.load(Ordering::SeqCst), "the file was read");
    }

    #[tokio::test]
    async fn an_overtaken_jump_moves_nothing() {
        let engine = TestEngine::new();
        let session = session(&["a", "b"]);
        engine.claims(NEWER);
        let played = jump_and_play(&engine, &session, 1, MOVE, async { Ok(()) }).await;
        assert!(played.is_ok());
        assert_eq!(current(&session).as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn running_off_the_end_stops_the_player() {
        let engine = TestEngine::new();
        let session = session(&["a"]);
        let played = step(&engine, &session, async { Ok(()) }).await;
        assert!(matches!(played, Ok(false)), "{played:?}");
        assert_eq!(engine.stops(), 1);
        assert!(!session.is_active());
    }

    /// Nothing else took over: no cursor is left on a file nothing plays.
    #[tokio::test]
    async fn a_failed_step_still_its_own_stops_and_drops_the_session() {
        let engine = TestEngine::new();
        let session = session(&["a", "b"]);
        let played = step(&engine, &session, async { Err(unreadable()) }).await;
        assert!(played.is_err());
        assert_eq!(engine.stops(), 1);
        assert!(!session.is_active());
    }

    #[tokio::test]
    async fn a_failed_jump_still_its_own_stops_and_drops_the_session() {
        let engine = TestEngine::new();
        let session = session(&["a", "b"]);
        let played = jump_and_play(&engine, &session, 1, MOVE, async { Err(unreadable()) }).await;
        assert!(played.is_err());
        assert_eq!(engine.stops(), 1);
        assert!(!session.is_active());
    }

    /// Unlike the remote queue: reading a file is local and quick, so the lock
    /// stays held, and nothing can overtake a move while it reads.
    #[tokio::test]
    async fn the_file_is_read_under_the_publish_lock() {
        let engine = TestEngine::new();
        let session = session(&["a", "b"]);
        let played = step(&engine, &session, async {
            assert!(engine.publish.try_lock().is_err(), "the lock was let go");
            assert_eq!(current(&session).as_deref(), Some("b"));
            Ok(())
        })
        .await;
        assert!(matches!(played, Ok(true)), "{played:?}");
    }
}
