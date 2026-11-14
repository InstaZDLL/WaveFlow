use super::super::replay_gain::TrackGain;
use super::resume::{resume_start_ms, LastLoadSource, LivePosition};
use super::*;
use std::path::PathBuf;
use waveflow_core::playback::stream::ActiveStream;

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
    // `Unload` above all: it is what a rebuild sends just before asking
    // for this snapshot back, and clearing on it would leave every
    // rebuild with nothing to resume.
    for cmd in [
        AudioCmd::Pause,
        AudioCmd::Resume,
        AudioCmd::Stop,
        AudioCmd::Unload,
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
        match (ParkedSession {
            load,
            start_ms,
            seeked: false,
        })
        .into_command(LoadIntent::from_raw(2))
        {
            AudioCmd::LoadAndPlay { start_ms, .. } => start_ms,
            _ => panic!("a library track resumes as LoadAndPlay"),
        }
    };
    assert_eq!(park(199_000), 0);
    assert_eq!(park(60_000), 60_000);
}

fn parked_at(start_ms: u64) -> Option<ParkedSession> {
    // Long enough that neither position falls in the last two seconds,
    // which a resume starts over from.
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
    Some(ParkedSession {
        load,
        start_ms,
        seeked: false,
    })
}

fn resumes_at(parked: Option<ParkedSession>) -> u64 {
    match parked
        .expect("still parked")
        .into_command(LoadIntent::from_raw(2))
    {
        AudioCmd::LoadAndPlay { start_ms, .. } => start_ms,
        _ => panic!("a library track resumes as LoadAndPlay"),
    }
}

#[test]
fn a_seek_moves_a_parked_session() {
    // Nothing is loaded while a session is parked, so a `Seek` sent to the
    // decoder was dropped and Play brought the track back where it was
    // parked: leaving a music video put the track back at the moment the
    // clip was opened (#1073).
    let mut parked = parked_at(10_000);
    assert!(super::resume::move_parked(&mut parked, false, 60_000));
    assert_eq!(resumes_at(parked), 60_000);
}

#[test]
fn a_seek_into_the_last_two_seconds_is_kept() {
    // A park left there starts over (#898); a seek the user asked for is
    // a choice, and leaving a clip that ran past the song puts the track
    // just before its end on purpose.
    let mut parked = parked_at(10_000);
    assert!(super::resume::move_parked(&mut parked, false, 199_500));
    assert_eq!(resumes_at(parked), 199_500);
}

#[test]
fn a_seek_leaves_a_superseded_park_alone() {
    // The user has moved on to another load: the seek is that load's.
    let mut parked = parked_at(10_000);
    assert!(!super::resume::move_parked(&mut parked, true, 60_000));
    assert_eq!(resumes_at(parked), 10_000);
}

#[test]
fn a_seek_with_nothing_parked_goes_to_the_decoder() {
    let mut parked = None;
    assert!(!super::resume::move_parked(&mut parked, false, 60_000));
    assert!(parked.is_none());
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

/// A short stereo WAV, so a hand-off has a real stream to swap in.
fn write_wav(path: &std::path::Path) {
    let (rate, frames, channels) = (44_100_u32, 4_410_u32, 2_u16);
    let data_len = frames * u32::from(channels) * 2;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data_len).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend((channels * 2).to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data_len.to_le_bytes());
    bytes.resize(bytes.len() + data_len as usize, 0);
    std::fs::write(path, bytes).unwrap();
}

/// The stream `SetNextTrack` would have prepared for `track_id`.
fn prepared(path: &std::path::Path, track_id: i64, gain_db: f64) -> ActiveStream {
    ActiveStream::open(
        path,
        track_id,
        200_000,
        "playlist".into(),
        Some(11),
        TrackGain {
            gain_db: Some(gain_db),
            ..Default::default()
        },
        0,
        false,
    )
    .expect("the prepared stream opens")
}

/// The decoder's side of a hand-off: what it records, then the track it
/// marks as started.
fn hand_off(shared: &SharedPlayback, next: &ActiveStream, start_ms: u64) {
    shared.record_handed_off_load(next, start_ms);
    shared
        .current_track_id
        .store(next.track_id, std::sync::atomic::Ordering::Release);
}

fn rebuild_live(shared: &SharedPlayback, position_ms: u64) -> LivePosition {
    LivePosition {
        intent: shared.last_load_intent(),
        track_id: shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire),
        position_ms,
    }
}

/// The reported case (#1103): a track resumed after a rebuild, then a run
/// of tracks the decoder moved through on its own — gapless, then a
/// crossfade — and a second rebuild. It used to put back the track the
/// run started from, at the position of the first resume.
#[test]
fn a_rebuild_after_hand_offs_resumes_the_track_playing_now() {
    let dir = tempfile::tempdir().unwrap();
    let (gapless, faded) = (dir.path().join("66.wav"), dir.path().join("42.wav"));
    write_wav(&gapless);
    write_wav(&faded);

    let shared = SharedPlayback::new();
    shared.record_accepted_load(&local_cmd(168, 9, 164_769));
    shared
        .current_track_id
        .store(168, std::sync::atomic::Ordering::Release);

    hand_off(&shared, &prepared(&gapless, 66, -3.0), 0);
    let load = shared.last_load().expect("a load on record");
    assert_eq!(load.track_id, 66);
    assert_eq!(load.start_ms, 0);

    hand_off(&shared, &prepared(&faded, 42, -6.0), 4_000);
    let load = shared.last_load().expect("a load on record");
    assert_eq!(load.track_id, 42);
    assert_eq!(load.start_ms, 4_000, "the fade was already 4 s into it");
    assert_eq!(
        load.intent,
        LoadIntent::from_raw(9),
        "no load arrived: the intent on record is still the one the decoder holds"
    );

    // The rebuild pairs the live position with the track playing now,
    // and sends that track back, whole, under the intent on record.
    let live = rebuild_live(&shared, 31_000);
    assert_eq!(resume_start_ms(&load, live), 31_000);
    match load.into_command(LoadIntent::from_raw(9), 31_000) {
        AudioCmd::LoadAndPlay {
            intent,
            path,
            start_ms,
            track_id,
            duration_ms,
            source_type,
            source_id,
            replay_gain,
        } => {
            assert_eq!(intent, LoadIntent::from_raw(9));
            assert_eq!(path, faded);
            assert_eq!(start_ms, 31_000);
            assert_eq!(track_id, 42);
            assert_eq!(duration_ms, 200_000);
            assert_eq!(source_type, "playlist");
            assert_eq!(source_id, Some(11));
            assert_eq!(replay_gain.gain_db, Some(-6.0));
        }
        other => panic!("expected a library load, got {other:?}"),
    }
}

/// A gapless hand-off out of a stream: the next track is a library one,
/// and the rebuild resumes it, not the station.
#[test]
fn a_hand_off_out_of_a_stream_records_a_library_track() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("next.wav");
    write_wav(&path);

    let shared = SharedPlayback::new();
    shared.record_accepted_load(&url_cmd("https://server.invalid/stream", -4, 3));
    hand_off(&shared, &prepared(&path, 5, 0.0), 0);

    let load = shared.last_load().expect("a load on record");
    assert_eq!(load.intent, LoadIntent::from_raw(3));
    assert_eq!(load.track_id, 5);
    assert_eq!(load.title, None);
    assert!(matches!(
        load.source,
        LastLoadSource::Local { path: ref p, .. } if p == &path
    ));
}

/// A newer pick still outranks what a hand-off recorded: the hand-off
/// claims nothing.
#[test]
fn a_pick_after_a_hand_off_still_wins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("next.wav");
    write_wav(&path);

    let shared = SharedPlayback::new();
    shared.record_accepted_load(&local_cmd(1, 4, 0));
    hand_off(&shared, &prepared(&path, 2, 0.0), 0);
    shared.record_accepted_load(&local_cmd(3, 5, 0));

    let load = shared.last_load().expect("a load on record");
    assert_eq!((load.track_id, load.intent), (3, LoadIntent::from_raw(5)));
}

/// A stream with no path to reopen, or no load to continue, leaves
/// nothing on record rather than the track before.
#[test]
fn a_hand_off_it_cannot_describe_clears_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("next.wav");
    write_wav(&path);
    let from_bytes = ActiveStream::open_from_source(
        Box::new(std::fs::File::open(&path).unwrap()),
        Some("wav"),
        2,
        1_000,
        "album".into(),
        None,
        TrackGain::default(),
    )
    .unwrap();

    let shared = SharedPlayback::new();
    shared.record_accepted_load(&local_cmd(1, 4, 0));
    shared.record_handed_off_load(&from_bytes, 0);
    assert_eq!(shared.last_load(), None);

    let fresh = SharedPlayback::new();
    fresh.record_handed_off_load(&prepared(&path, 2, 0.0), 0);
    assert_eq!(fresh.last_load(), None);
}
