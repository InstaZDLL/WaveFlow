use std::time::{Duration, Instant};

use super::{TailDrain, TAIL_POLL, TAIL_STALL_LIMIT};

/// Poll every [`TAIL_POLL`] with nothing taken from the ring, from
/// `from` for `watched`; the answer at the last poll.
fn watch_stalled(slot: &mut Option<TailDrain>, from: Instant, watched: Duration) -> bool {
    let mut t = from;
    let mut playing = true;
    while t.duration_since(from) < watched {
        t += TAIL_POLL;
        playing = TailDrain::still_playing(slot, 50_000, false, t);
    }
    playing
}

#[test]
fn an_empty_ring_ends_the_track() {
    let mut slot = None;
    assert!(!TailDrain::still_playing(
        &mut slot,
        0,
        false,
        Instant::now()
    ));
    assert!(slot.is_none());
}

#[test]
fn the_end_plays_out_while_the_output_takes_it() {
    let start = Instant::now();
    let mut slot = None;
    assert!(TailDrain::still_playing(&mut slot, 96_000, false, start));
    // Progress, however long it takes in all: still the end playing.
    let later = start + TAIL_STALL_LIMIT * 2;
    assert!(TailDrain::still_playing(&mut slot, 40_000, false, later));
    assert!(TailDrain::still_playing(
        &mut slot,
        1_000,
        false,
        later + TAIL_STALL_LIMIT / 2
    ));
    assert!(!TailDrain::still_playing(
        &mut slot,
        0,
        false,
        later + TAIL_STALL_LIMIT
    ));
}

#[test]
fn a_stall_watched_for_the_limit_gives_the_end_up() {
    // Playing, and the output takes nothing: a lost device.
    let start = Instant::now();
    let mut slot = None;
    assert!(TailDrain::still_playing(&mut slot, 50_000, false, start));
    assert!(watch_stalled(&mut slot, start, TAIL_STALL_LIMIT / 2));
    let mut slot = None;
    assert!(TailDrain::still_playing(&mut slot, 50_000, false, start));
    assert!(!watch_stalled(&mut slot, start, TAIL_STALL_LIMIT));
}

#[test]
fn a_pause_holds_the_end() {
    let start = Instant::now();
    let mut slot = None;
    assert!(TailDrain::still_playing(&mut slot, 50_000, false, start));
    // Polled while paused: the end waits for the user, however long.
    let mut t = start;
    for _ in 0..1_000 {
        t += TAIL_POLL * 10;
        assert!(TailDrain::still_playing(&mut slot, 50_000, true, t));
    }
}

#[test]
fn time_away_from_the_loop_is_not_a_stall() {
    // A pause blocks the decoder in `drain_commands` until Resume: the
    // ring is not looked at meanwhile, and that minute is not the
    // output failing to take samples. Resumed, the end plays on.
    let start = Instant::now();
    let mut slot = None;
    assert!(TailDrain::still_playing(&mut slot, 50_000, false, start));
    let resumed = start + Duration::from_secs(60);
    assert!(TailDrain::still_playing(&mut slot, 50_000, false, resumed));
    assert!(TailDrain::still_playing(
        &mut slot,
        40_000,
        false,
        resumed + TAIL_POLL
    ));
    // And a real stall after it is still caught.
    assert!(!watch_stalled(
        &mut slot,
        resumed + TAIL_POLL,
        TAIL_STALL_LIMIT
    ));
}
