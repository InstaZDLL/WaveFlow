use super::*;
use std::path::PathBuf;

fn url_cmd(url: &str, track_id: i64, intent: u64) -> AudioCmd {
    AudioCmd::LoadUrlAndPlay {
        intent: LoadIntent::from_raw(intent),
        url: url.to_string(),
        ext_hint: Some("mp3".to_string()),
        track_id,
        title: Some("Test stream".to_string()),
        artist: Some("Test artist".to_string()),
        artwork_url: Some("https://example.invalid/art.jpg".to_string()),
        cache: None,
        duration_ms: None,
        seekable_file: false,
        replay_gain: TrackGain::default(),
    }
}

fn local_cmd(track_id: i64, intent: u64, start_ms: u64) -> AudioCmd {
    AudioCmd::LoadAndPlay {
        intent: LoadIntent::from_raw(intent),
        path: PathBuf::from("/dev/null"),
        start_ms,
        track_id,
        duration_ms: 1000,
        source_type: "album".into(),
        source_id: Some(7),
        replay_gain: TrackGain::default(),
    }
}

fn remote_local_cmd(fallback_url: Option<&str>, track_id: i64) -> AudioCmd {
    AudioCmd::LoadRemoteFileAndPlay {
        intent: LoadIntent::from_raw(1),
        origin: DetachedOrigin::Remote,
        path: PathBuf::from("/dev/null"),
        start_ms: 0,
        track_id,
        duration_ms: 1000,
        title: Some("Remote track".to_string()),
        artist: Some("Remote artist".to_string()),
        artwork_url: None,
        fallback_url: fallback_url.map(str::to_string),
        discard_on_failure: false,
        replay_gain: TrackGain {
            gain_db: Some(-4.0),
            peak: None,
            peak_unverified: false,
            ..Default::default()
        },
    }
}

fn live(intent: u64, track_id: i64, position_ms: u64) -> LivePosition {
    LivePosition {
        intent: Some(LoadIntent::from_raw(intent)),
        track_id,
        position_ms,
    }
}

#[test]
fn a_url_load_is_captured_verbatim() {
    let snap = LastLoad::capture(&url_cmd("https://radio.invalid/live", -1, 1))
        .expect("a load is captured");
    assert_eq!(snap.track_id, -1);
    assert_eq!(snap.title.as_deref(), Some("Test stream"));
    match snap.source {
        LastLoadSource::Url { url, ext_hint, .. } => {
            assert_eq!(url, "https://radio.invalid/live");
            assert_eq!(ext_hint.as_deref(), Some("mp3"));
        }
        _ => panic!("expected a URL load"),
    }
}

#[test]
fn a_library_track_is_captured_too() {
    // It was not, before #634: the snapshot existed only to resume
    // radio, and a local `LoadAndPlay` wiped it. The rebuild then went
    // to the database for the track it had noted the id of — which is
    // exactly the older snapshot that left nothing playing.
    let snap = LastLoad::capture(&local_cmd(42, 3, 0)).expect("a load is captured");
    assert_eq!(snap.track_id, 42);
    assert!(matches!(
        snap.source,
        LastLoadSource::Local { ref path, .. } if path == &PathBuf::from("/dev/null")
    ));
}

#[test]
fn nothing_but_a_load_is_captured() {
    // `Stop` above all: it is what a rebuild sends just before asking
    // for this snapshot back, and clearing on it would leave every
    // rebuild with nothing to resume.
    for cmd in [
        AudioCmd::Pause,
        AudioCmd::Resume,
        AudioCmd::Stop,
        AudioCmd::Seek(123),
        AudioCmd::SetVolume(0.5),
        AudioCmd::SetMono(true),
    ] {
        assert!(
            LastLoad::capture(&cmd).is_none(),
            "{cmd:?} is not a load and must leave the snapshot alone"
        );
    }
}

#[test]
fn the_resume_carries_the_load_s_own_intent() {
    // The heart of #634. A fresh intent would rank this resume above a
    // pick that claimed before it and has not reached the channel yet;
    // the original is accepted again — the decoder drops what is
    // *older* than the newest it has been handed — and gives way to a
    // genuinely newer one.
    let snap = LastLoad::capture(&local_cmd(42, 9, 0)).expect("a load is captured");
    let intent = snap.intent;
    assert_eq!(intent, LoadIntent::from_raw(9));
    assert_eq!(
        snap.into_command(intent, 1_000).load_intent(),
        Some(LoadIntent::from_raw(9))
    );
}

#[test]
fn a_park_at_the_very_end_resumes_from_the_start() {
    let park = |start_ms| {
        let cmd = AudioCmd::LoadAndPlay {
            intent: LoadIntent::from_raw(1),
            path: PathBuf::from("/dev/null"),
            start_ms: 0,
            track_id: 42,
            duration_ms: 200_000,
            source_type: "album".into(),
            source_id: Some(7),
            replay_gain: TrackGain::default(),
        };
        let load = LastLoad::capture(&cmd).expect("a load is captured");
        match (ParkedSession { load, start_ms }).into_command(LoadIntent::from_raw(2)) {
            AudioCmd::LoadAndPlay { start_ms, .. } => start_ms,
            _ => panic!("a library track resumes as LoadAndPlay"),
        }
    };
    assert_eq!(park(199_000), 0);
    assert_eq!(park(60_000), 60_000);
}

#[test]
fn a_resumed_library_track_keeps_the_source_it_came_from() {
    // Stamping "device-rebuild" on it, as the three paths used to,
    // hides an album play from every statistic that filters on the
    // source — for the sole reason that the audio device changed.
    let snap = LastLoad::capture(&local_cmd(42, 1, 0)).expect("a load is captured");
    let intent = snap.intent;
    match snap.into_command(intent, 2_500) {
        AudioCmd::LoadAndPlay {
            source_type,
            source_id,
            start_ms,
            track_id,
            ..
        } => {
            assert_eq!(source_type, "album");
            assert_eq!(source_id, Some(7));
            assert_eq!(start_ms, 2_500);
            assert_eq!(track_id, 42);
        }
        _ => panic!("a library track resumes as LoadAndPlay"),
    }
}

#[test]
fn online_device_change_reemits_remote_local_command_with_fallback() {
    let snap = LastLoad::capture(&remote_local_cmd(Some("https://server.invalid/stream"), -3))
        .expect("a load is captured");
    let intent = snap.intent;
    match snap.into_command(intent, 4_321) {
        AudioCmd::LoadRemoteFileAndPlay {
            path,
            start_ms,
            track_id,
            duration_ms,
            fallback_url,
            replay_gain,
            discard_on_failure,
            ..
        } => {
            assert_eq!(path, PathBuf::from("/dev/null"));
            assert_eq!(start_ms, 4_321);
            assert_eq!(track_id, -3);
            assert_eq!(duration_ms, 1_000);
            assert_eq!(
                fallback_url.as_deref(),
                Some("https://server.invalid/stream")
            );
            assert_eq!(replay_gain.gain_db, Some(-4.0));
            assert!(
                !discard_on_failure,
                "a resume never authorises deleting the file it re-opens"
            );
        }
        _ => panic!("device change must resume the reconciled local file"),
    }
}

#[test]
fn offline_device_change_reemits_remote_local_command_without_fallback() {
    let snap = LastLoad::capture(&remote_local_cmd(None, -3)).expect("a load is captured");
    let intent = snap.intent;
    match snap.into_command(intent, 7_654) {
        AudioCmd::LoadRemoteFileAndPlay {
            path,
            start_ms,
            fallback_url,
            ..
        } => {
            assert_eq!(path, PathBuf::from("/dev/null"));
            assert_eq!(start_ms, 7_654);
            assert_eq!(fallback_url, None);
        }
        _ => panic!("offline device change must still resume locally"),
    }
}

#[test]
fn the_position_is_kept_for_the_load_it_belongs_to() {
    let snap = LastLoad::capture(&local_cmd(42, 5, 0)).expect("a load is captured");
    assert_eq!(resume_start_ms(&snap, live(5, 42, 90_000)), 90_000);
}

#[test]
fn a_track_picked_during_the_rebuild_starts_at_its_own_beginning() {
    // The position was read while track 42 was playing; what the
    // decoder accepted since is track 77, at 0. Handing it 42's
    // position would drop the user into the middle of a track they
    // just picked — or past its end.
    let snap = LastLoad::capture(&local_cmd(77, 6, 0)).expect("a load is captured");
    assert_eq!(resume_start_ms(&snap, live(5, 42, 90_000)), 0);
}

#[test]
fn re_picking_the_same_track_restarts_it() {
    // Same id, different intent: the user asked for the track again,
    // and resuming at the old position would quietly ignore that.
    let snap = LastLoad::capture(&local_cmd(42, 6, 0)).expect("a load is captured");
    assert_eq!(resume_start_ms(&snap, live(5, 42, 90_000)), 0);
}

#[test]
fn a_load_accepted_but_not_yet_started_uses_its_own_start() {
    // The window between "the decoder accepted this load" and "the
    // decoder started it": the intent already matches, the track id
    // does not yet, and the position still belongs to the previous
    // track. Both halves have to agree before the position is trusted.
    let snap = LastLoad::capture(&local_cmd(77, 5, 30_000)).expect("a load is captured");
    assert_eq!(resume_start_ms(&snap, live(5, 42, 90_000)), 30_000);
}

#[test]
fn a_decoder_with_no_load_on_record_pairs_with_nothing() {
    let snap = LastLoad::capture(&local_cmd(42, 5, 12_000)).expect("a load is captured");
    let live = LivePosition {
        intent: None,
        track_id: 42,
        position_ms: 90_000,
    };
    assert_eq!(resume_start_ms(&snap, live), 12_000);
}
