use super::*;

/// The clock does not care how the speed is made: it scales by it either way.
#[test]
fn the_pitch_lock_and_the_transposition_leave_the_clock_alone() {
    let shared = SharedPlayback::new();
    shared.sample_rate.store(48_000, Ordering::Relaxed);
    shared.channels.store(2, Ordering::Relaxed);
    shared.set_playback_speed(1.5);
    shared.samples_played.store(96_000, Ordering::Relaxed);
    let position = shared.current_position_ms();
    assert_eq!(position, 1_500);
    shared.keep_pitch.store(false, Ordering::Release);
    assert_eq!(shared.current_position_ms(), position);
    shared.set_pitch_semitones(4);
    assert_eq!(shared.current_position_ms(), position);
    assert_eq!(shared.session_listened_ms(), 1_500);
}

/// A change of speed or pitch restarts the clock, not the listen: what was heard
/// stays heard, at the speed it played at, or the end of the track earns no
/// credit. A seek or a new track does start over.
#[test]
fn a_change_of_speed_or_pitch_keeps_what_was_heard() {
    let shared = SharedPlayback::new();
    shared.sample_rate.store(48_000, Ordering::Relaxed);
    shared.channels.store(2, Ordering::Relaxed);
    // 10 s at 1×.
    shared.samples_played.store(960_000, Ordering::Relaxed);
    assert_eq!(shared.session_listened_ms(), 10_000);

    // Then 2 s at 1.5×, which is 3 s of track.
    shared.set_playback_speed(1.5);
    assert_eq!(shared.session_listened_ms(), 10_000);
    shared.samples_played.store(192_000, Ordering::Relaxed);
    assert_eq!(shared.session_listened_ms(), 13_000);

    // The decoder puts the file back at the position heard.
    let position = shared.current_position_ms();
    shared.rebase_clock(position);
    assert_eq!(shared.session_listened_ms(), 13_000);
    assert_eq!(shared.current_position_ms(), position);

    shared.restart_clock(0, 0, false);
    assert_eq!(shared.session_listened_ms(), 0);
}

#[test]
fn the_transposition_is_clamped() {
    let shared = SharedPlayback::new();
    shared.set_pitch_semitones(40);
    assert_eq!(
        shared.pitch_semitones.load(Ordering::Relaxed),
        MAX_PITCH_SEMITONES
    );
    shared.set_pitch_semitones(-40);
    assert_eq!(
        shared.pitch_semitones.load(Ordering::Relaxed),
        -MAX_PITCH_SEMITONES
    );
}

/// The three-state rule behind automatic ReplayGain (#587 × #618),
/// and the regression it exists for.
///
/// Track shuffle is the case a boolean for "album grouping" got
/// wrong: an album queue that has been shuffled track-by-track
/// still carries `source_type = 'album'` on every row, so reading
/// only the source said "record playing through" about a record
/// that had just been taken apart.
///
/// It was not reachable before this branch, either — a reorder
/// used to flatten `source_type` to `'manual'`, so the right
/// answer came out by accident. Preserving the column is what
/// exposed it.
#[test]
fn the_listening_context_reads_all_three_shuffle_states() {
    use super::super::replay_gain::Listening;
    use crate::queue::ShuffleMode;

    let shared = SharedPlayback::new();
    let set = |mode: ShuffleMode| {
        shared
            .shuffle_mode_bits
            .store(mode.as_bits(), Ordering::Relaxed);
    };

    // Off: the queue plays as built, so its own source decides.
    set(ShuffleMode::Off);
    assert_eq!(shared.listening_to("album"), Listening::ToAnAlbum);
    assert_eq!(shared.listening_to("playlist"), Listening::ToATrack);
    assert_eq!(shared.listening_to("library"), Listening::ToATrack);

    // Albums: a record is playing through whatever the queue was
    // built from.
    set(ShuffleMode::Albums);
    assert_eq!(shared.listening_to("album"), Listening::ToAnAlbum);
    assert_eq!(shared.listening_to("playlist"), Listening::ToAnAlbum);

    // Tracks: the record has been taken apart. This is the one
    // that used to be wrong.
    set(ShuffleMode::Tracks);
    assert_eq!(
        shared.listening_to("album"),
        Listening::ToATrack,
        "a shuffled album is not a record playing through"
    );
    assert_eq!(shared.listening_to("playlist"), Listening::ToATrack);
}

/// A Mood Radio or Daily Mix built out of whole records plays them
/// in disc order under a `source_type` that cannot say so (#647).
#[test]
fn a_generator_session_of_whole_records_is_a_record_playing_through() {
    use super::super::replay_gain::Listening;
    use crate::queue::ShuffleMode;

    let shared = SharedPlayback::new();
    shared
        .shuffle_mode_bits
        .store(ShuffleMode::Off.as_bits(), Ordering::Relaxed);

    assert_eq!(
        shared.listening_to("radio"),
        Listening::ToATrack,
        "a mood radio of individual tracks is exactly that"
    );

    shared.album_ordered_queue.store(true, Ordering::Relaxed);
    assert_eq!(shared.listening_to("radio"), Listening::ToAnAlbum);
    assert_eq!(
        shared.listening_to("playlist"),
        Listening::ToAnAlbum,
        "an album-mode Daily Mix plays as a stored playlist"
    );

    assert_eq!(
        shared.listening_to("manual"),
        Listening::ToATrack,
        "a track wedged in by hand is not part of the record it landed beside"
    );

    // And shuffle still decides before it does: a session of whole
    // records taken apart track by track is not one any more.
    shared
        .shuffle_mode_bits
        .store(ShuffleMode::Tracks.as_bits(), Ordering::Relaxed);
    assert_eq!(shared.listening_to("radio"), Listening::ToATrack);
}

/// A fresh engine has shuffle off, so nothing claims to be an
/// album before a profile is loaded.
#[test]
fn a_new_shared_state_starts_with_shuffle_off() {
    use crate::queue::ShuffleMode;
    let shared = SharedPlayback::new();
    assert!(
        !shared.album_ordered_queue.load(Ordering::Relaxed),
        "and no queue of records either"
    );
    assert_eq!(
        ShuffleMode::from_bits(shared.shuffle_mode_bits.load(Ordering::Relaxed)),
        ShuffleMode::Off
    );
}

#[test]
fn position_zero_when_idle() {
    // Right after construction nothing has been written: position
    // is just the (zero) base offset — must not divide-by-zero on
    // the empty sample_rate / channels fields.
    let s = SharedPlayback::new();
    assert_eq!(s.current_position_ms(), 0);
    assert_eq!(s.session_listened_ms(), 0);
}

#[test]
fn position_combines_base_offset_and_played_samples() {
    // 44.1 kHz stereo, 44_100 frames played → 1000 ms of audio
    // delivered. With a 5_000 ms base offset (a resume point), the
    // wall-clock track position is 6_000 ms.
    let s = SharedPlayback::new();
    s.sample_rate.store(44_100, Ordering::Relaxed);
    s.channels.store(2, Ordering::Relaxed);
    // samples_played counts interleaved frames * channels.
    s.samples_played.store(44_100 * 2, Ordering::Relaxed);
    s.base_offset_ms.store(5_000, Ordering::Relaxed);

    assert_eq!(s.current_position_ms(), 6_000);
    // session counter ignores the base offset on purpose.
    assert_eq!(s.session_listened_ms(), 1_000);
}

#[test]
fn the_decoder_is_a_ring_ahead_of_what_was_heard() {
    // 44.1 kHz stereo: 1 s heard, 1 s more queued in the ring. The
    // decoder has reached 2 s; the triggers timed on the end of what
    // it decodes must read that, not the 1 s heard (#932).
    let s = SharedPlayback::new();
    s.sample_rate.store(44_100, Ordering::Relaxed);
    s.channels.store(2, Ordering::Relaxed);
    s.samples_played.store(44_100 * 2, Ordering::Relaxed);
    assert_eq!(s.current_position_ms(), 1_000);
    assert_eq!(s.decoded_position_ms(44_100 * 2), 2_000);
    assert_eq!(s.decoded_position_ms(0), 1_000);

    // At double speed a second of output holds two of the track.
    s.set_playback_speed(2.0);
    assert_eq!(
        s.decoded_position_ms(44_100 * 2) - s.current_position_ms(),
        2_000
    );
}

#[test]
fn the_finished_tracks_tail_is_not_the_new_tracks_after_a_gapless_swap() {
    // A gapless swap with 1 s of the finished track still queued: the
    // new track has decoded nothing yet, so the decoder is at 0, not at
    // the 1 s the queue holds.
    let s = SharedPlayback::new();
    s.sample_rate.store(44_100, Ordering::Relaxed);
    s.channels.store(2, Ordering::Relaxed);
    let tail = 44_100 * 2;
    s.restart_clock(0, tail, true);
    assert_eq!(s.decoded_position_ms(tail), 0);

    // Half a second of the new track written behind the tail.
    assert_eq!(s.decoded_position_ms(tail + 22_050 * 2), 500);

    // The output played the tail: still 500 ms decoded, now all of it
    // the new track's, and none of it heard yet.
    s.samples_played.store(tail, Ordering::Relaxed);
    assert_eq!(s.decoded_position_ms(22_050 * 2), 500);
    assert_eq!(s.current_position_ms(), 0);
}

#[test]
fn crossfade_restart_counts_the_fade_and_not_the_queued_tail() {
    // A 5 s crossfade ends with 0.25 s of the mix still in the ring.
    // The new track is already 4.75 s in as heard, and reaches 5 s
    // once the output has played that tail. Restarting at 0 is what
    // left lyrics and the seek bar a fade behind the audio.
    let s = SharedPlayback::new();
    s.sample_rate.store(44_100, Ordering::Relaxed);
    s.channels.store(2, Ordering::Relaxed);
    let queued = 11_025 * 2; // 250 ms of interleaved stereo
    s.restart_clock(5_000, queued, false);
    assert_eq!(s.current_position_ms(), 4_750);
    assert_eq!(s.session_listened_ms(), 0);

    // That tail is the new track, heard: it counts as listening.
    s.samples_played.store(queued, Ordering::Relaxed);
    assert_eq!(s.current_position_ms(), 5_000);
    assert_eq!(s.session_listened_ms(), 250);

    s.samples_played
        .store(queued + 44_100 * 2, Ordering::Relaxed);
    assert_eq!(s.current_position_ms(), 6_000);
    assert_eq!(s.session_listened_ms(), 1_250);
}

#[test]
fn gapless_restart_holds_at_zero_while_the_previous_tail_plays() {
    // A gapless hand-off leaves the end of the finished track in the
    // ring: the new one must read 0 until that has been played, not
    // run ahead of the audio by the ring's length.
    let s = SharedPlayback::new();
    s.sample_rate.store(48_000, Ordering::Relaxed);
    s.channels.store(8, Ordering::Relaxed);
    let queued = 12_000 * 8; // 250 ms at 48 kHz, 8 channels
    s.restart_clock(0, queued, true);
    s.samples_played.store(queued / 2, Ordering::Relaxed);
    assert_eq!(s.current_position_ms(), 0);
    assert_eq!(s.session_listened_ms(), 0);

    s.samples_played
        .store(queued + 48_000 * 8, Ordering::Relaxed);
    assert_eq!(s.current_position_ms(), 1_000);
    assert_eq!(s.session_listened_ms(), 1_000);
}

#[test]
fn a_seek_after_a_hand_off_clears_the_lead_in() {
    // A seek empties the ring, so nothing queued may be subtracted
    // from the position it restarts at.
    let s = SharedPlayback::new();
    s.sample_rate.store(44_100, Ordering::Relaxed);
    s.channels.store(2, Ordering::Relaxed);
    s.restart_clock(5_000, 44_100, false);
    s.restart_clock(30_000, 0, false);
    assert_eq!(s.current_position_ms(), 30_000);
}

#[test]
fn speed_change_during_the_lead_in_keeps_the_position() {
    // The speed rebase snapshots the position with the lead-in
    // already applied, so it must not subtract it a second time.
    let s = SharedPlayback::new();
    s.sample_rate.store(44_100, Ordering::Relaxed);
    s.channels.store(2, Ordering::Relaxed);
    s.restart_clock(5_000, 11_025 * 2, false);
    s.set_playback_speed(1.5);
    assert_eq!(s.current_position_ms(), 4_750);
}

#[test]
fn state_round_trips_through_atomic() {
    let s = SharedPlayback::new();
    for state in [
        PlayerState::Idle,
        PlayerState::Loading,
        PlayerState::Playing,
        PlayerState::Paused,
        PlayerState::Ended,
    ] {
        s.set_state(state);
        assert_eq!(s.state(), state);
    }
}

#[test]
fn volume_clamps_to_unit_range() {
    let s = SharedPlayback::new();
    s.set_volume(2.5);
    assert_eq!(s.volume(), 1.0);
    s.set_volume(-1.0);
    assert_eq!(s.volume(), 0.0);
    s.set_volume(0.5);
    assert_eq!(s.volume(), 0.5);
}

#[test]
fn playback_speed_clamps_to_supported_range() {
    let s = SharedPlayback::new();
    s.set_playback_speed(5.0);
    assert_eq!(s.playback_speed(), 2.0);
    s.set_playback_speed(0.1);
    assert_eq!(s.playback_speed(), 0.5);
    s.set_playback_speed(1.5);
    assert!((s.playback_speed() - 1.5).abs() < 1e-6);
}

#[test]
fn position_scales_with_playback_speed() {
    // At 2× speed, the cpal callback emits half as many output
    // samples per second of track audio — so 22_050 stereo frames
    // pushed into the ring (= 0.5 s wall-clock at 44.1 kHz)
    // correspond to 1.0 s of track position.
    //
    // Speed must be set BEFORE the simulated samples_played
    // counter is advanced. `set_playback_speed` snapshots the
    // pre-change position and rebases samples_played to keep
    // position continuous; setting speed last would zero out the
    // counter we just primed.
    let s = SharedPlayback::new();
    s.sample_rate.store(44_100, Ordering::Relaxed);
    s.channels.store(2, Ordering::Relaxed);
    s.set_playback_speed(2.0);
    s.samples_played.store(22_050 * 2, Ordering::Relaxed);
    assert_eq!(s.current_position_ms(), 1_000);
    assert_eq!(s.session_listened_ms(), 1_000);
}

#[test]
fn speed_change_preserves_position_continuity() {
    // 1 s of audio at 1.0× speed (44_100 stereo frames played),
    // then the user flips to 2.0×. Position must NOT jump: we
    // were at 1000 ms before, we should still read 1000 ms
    // immediately after — the rebase resets samples_played and
    // moves the elapsed time into base_offset_ms.
    let s = SharedPlayback::new();
    s.sample_rate.store(44_100, Ordering::Relaxed);
    s.channels.store(2, Ordering::Relaxed);
    s.samples_played.store(44_100 * 2, Ordering::Relaxed);
    assert_eq!(s.current_position_ms(), 1_000);
    s.set_playback_speed(2.0);
    assert_eq!(s.current_position_ms(), 1_000);
}

/// Three minutes and a half into track 7, at 44.1 kHz stereo.
fn on_track_seven() -> SharedPlayback {
    let s = SharedPlayback::new();
    s.sample_rate.store(44_100, Ordering::Relaxed);
    s.channels.store(2, Ordering::Relaxed);
    s.samples_played.store(210 * 44_100 * 2, Ordering::Relaxed);
    s.current_track_id.store(7, Ordering::Release);
    s
}

/// A track that ran to its end is saved, and offered to Play, at its
/// start (#1109); one stopped or paused anywhere else keeps its position.
#[test]
fn a_track_that_ran_out_is_resumed_from_its_start() {
    let s = on_track_seven();
    s.set_state(PlayerState::Playing);
    assert_eq!(s.resume_point(), Some((7, 210_000)));
    assert_eq!(s.finished_track_id(), None);

    s.set_state(PlayerState::Ended);
    assert_eq!(s.resume_point(), Some((7, 0)));
    assert_eq!(s.finished_track_id(), Some(7));

    // A `Stop` on the ended track leaves it no less finished.
    s.set_state(PlayerState::Idle);
    assert_eq!(s.resume_point(), Some((7, 0)));
    assert_eq!(s.finished_track_id(), Some(7));
}

/// Whatever loads next is not finished, and neither is a track a `Stop`
/// put on `Idle` part-way through.
#[test]
fn the_next_load_or_a_stop_mid_track_is_not_finished() {
    for next in [
        PlayerState::Loading,
        PlayerState::Playing,
        PlayerState::Paused,
    ] {
        let s = on_track_seven();
        s.set_state(PlayerState::Ended);
        s.set_state(next);
        assert_eq!(s.finished_track_id(), None, "{next:?}");
        assert_eq!(s.resume_point(), Some((7, 210_000)), "{next:?}");
    }
    let s = on_track_seven();
    s.set_state(PlayerState::Playing);
    s.set_state(PlayerState::Idle);
    assert_eq!(s.finished_track_id(), None);
    assert_eq!(s.resume_point(), Some((7, 210_000)));
}

/// No library track, no resume point and nothing finished: radio and the
/// remote queue use negative ids, a profile switch clears the id to `0`.
#[test]
fn only_a_library_track_has_a_resume_point() {
    for track_id in [0, -1] {
        let s = on_track_seven();
        s.current_track_id.store(track_id, Ordering::Release);
        s.set_state(PlayerState::Ended);
        assert_eq!(s.resume_point(), None, "{track_id}");
        assert_eq!(s.finished_track_id(), None, "{track_id}");
    }
}

/// A pause is recent only while it lasts (#1158): playing again, or a
/// window that has run out, both answer no.
#[test]
fn a_pause_is_recent_only_while_it_lasts() {
    let shared = SharedPlayback::new();
    let window = std::time::Duration::from_secs(60);
    assert!(!shared.paused_within(window), "never paused");
    shared.set_state(PlayerState::Playing);
    shared.set_state(PlayerState::Paused);
    assert!(shared.paused_within(window));
    assert!(!shared.paused_within(std::time::Duration::ZERO));
    shared.set_state(PlayerState::Playing);
    assert!(!shared.paused_within(window), "playing again");
}

/// A resume, then a new pause whose moment is written but whose `Paused`
/// is not published yet: the moment belongs to no published pause, and
/// the answer is no until the state says `Paused` (#1158).
#[test]
fn a_pause_moment_counts_only_with_its_published_state() {
    let shared = SharedPlayback::new();
    let window = std::time::Duration::from_secs(60);
    shared.set_state(PlayerState::Paused);
    shared.set_state(PlayerState::Playing);
    // `set_state(Paused)` halfway: the moment first, the state not yet.
    shared
        .paused_at
        .store(shared.clock_now(), Ordering::Release);
    assert!(!shared.paused_within(window));
    shared.set_state(PlayerState::Paused);
    assert!(shared.paused_within(window));
}
