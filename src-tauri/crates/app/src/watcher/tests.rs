use super::*;
use notify::event::{AccessKind, CreateKind, Flag};

fn event(paths: &[&str]) -> Event {
    let mut event = Event::new(EventKind::Create(CreateKind::Any));
    event.paths = paths.iter().map(PathBuf::from).collect();
    event
}

/// Caching a clip must not cost a folder rescan; a prefetch across an
/// album would otherwise pay for one per clip.
#[test]
fn our_own_writes_do_not_trigger_a_scan() {
    let root = PathBuf::from("/music");
    assert!(is_our_own_media(
        &root,
        &event(&["/music/.waveflow/canvas/a.mp4"])
    ));
}

/// Every path, not any: an event that touches one of ours and one of
/// theirs is a library change, and theirs is the half that matters.
#[test]
fn an_event_that_also_touches_their_music_is_a_change() {
    let root = PathBuf::from("/music");
    assert!(!is_our_own_media(
        &root,
        &event(&["/music/.waveflow/canvas/a.mp4", "/music/Artist/01.flac"])
    ));
    assert!(!is_our_own_media(&root, &event(&["/music/Artist/01.flac"])));
}

/// An event naming no path cannot be judged, so it is kept: a scan too
/// many is recoverable, a change never seen is not.
#[test]
fn an_event_without_paths_is_kept() {
    assert!(!is_our_own_media(Path::new("/music"), &event(&[])));
}

/// The backend saying it lost events (inotify overflow, FSEvents
/// must-scan) is the one event that most needs a scan: it is what a
/// large copy produces.
#[test]
fn a_lost_events_signal_asks_for_a_scan() {
    let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
    assert!(is_relevant_event(&rescan));
    assert!(!is_our_own_media(Path::new("/music"), &rescan));
}

#[test]
fn reads_and_unflagged_others_do_not() {
    assert!(!is_relevant_event(&Event::new(EventKind::Access(
        AccessKind::Any
    ))));
    assert!(!is_relevant_event(&Event::new(EventKind::Other)));
    assert!(is_relevant_event(&event(&["/music/a.flac"])));
}

#[test]
fn errors_are_logged_once_a_minute_with_a_count() {
    let mut throttle = ErrorThrottle::default();
    let start = Instant::now();
    assert_eq!(throttle.admit(start), Some(0));
    assert_eq!(throttle.admit(start + Duration::from_secs(1)), None);
    assert_eq!(throttle.admit(start + Duration::from_secs(30)), None);
    assert_eq!(
        throttle.admit(start + ERROR_LOG_INTERVAL + Duration::from_secs(1)),
        Some(2)
    );
    assert_eq!(
        throttle.admit(start + ERROR_LOG_INTERVAL + Duration::from_secs(2)),
        None
    );
}
