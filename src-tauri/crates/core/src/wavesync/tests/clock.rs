use super::*;
use crate::wavesync::clock::{estimate_offset, follow, host_now, Follow, Local, Sample, DRIFT_MS};

/// A probe sent at `sent` on the guest, taking `up` ms to reach a host
/// whose clock is `offset` ahead, and `down` ms to come back.
fn probe(sent: u64, up: u64, down: u64, offset: i64) -> Sample {
    Sample {
        sent_ms: sent,
        host_ms: (sent + up).saturating_add_signed(offset),
        received_ms: sent + up + down,
    }
}

#[test]
fn the_hosts_clock_is_found_from_the_fastest_probes() {
    let offset = 1_234_567;
    // Symmetric trips: exact.
    let even: Vec<Sample> = (0..5)
        .map(|i| probe(1_000 + i * 100, 4, 4, offset))
        .collect();
    assert_eq!(estimate_offset(&even), Some(offset));

    // Two probes waited 300 ms on the way back: believing them would put the
    // host 150 ms early. The fast half outvotes them.
    let mut noisy = even.clone();
    noisy.push(probe(2_000, 4, 300, offset));
    noisy.push(probe(2_100, 4, 300, offset));
    assert_eq!(estimate_offset(&noisy), Some(offset));

    // A host behind the guest.
    assert_eq!(
        estimate_offset(&[probe(50_000, 2, 2, -40_000)]),
        Some(-40_000)
    );

    // An answer that came back before it left is no sample.
    let broken = Sample {
        sent_ms: 10,
        host_ms: 5,
        received_ms: 9,
    };
    assert_eq!(estimate_offset(&[broken]), None);
    assert_eq!(estimate_offset(&[]), None);
}

#[test]
fn the_hosts_now_never_wraps() {
    assert_eq!(host_now(1_000, 500), 1_500);
    assert_eq!(host_now(1_000, -400), 600);
    assert_eq!(host_now(1_000, -5_000), 0);
    assert_eq!(host_now(u64::MAX - 1, 10), u64::MAX);
}

fn anchor(entry: Option<u8>, position_ms: u64, at_ms: u64, playing: bool) -> Anchor {
    Anchor {
        entry: entry.map(id),
        position_ms,
        at_ms,
        playing,
    }
}

fn local(entry: Option<u8>, position_ms: u64, playing: bool, buffering: bool) -> Local {
    Local {
        entry: entry.map(id),
        position_ms,
        playing,
        buffering,
    }
}

#[test]
fn a_guest_follows_what_the_host_plays() {
    let playing = anchor(Some(1), 10_000, 50_000, true);
    // 2 s after the anchor, the host is at 12 s.
    let now = 52_000;

    assert_eq!(
        follow(&playing, now, &local(None, 0, false, false)),
        Follow::Load {
            entry: id(1),
            position_ms: 12_000,
            playing: true
        }
    );
    assert_eq!(
        follow(&playing, now, &local(Some(2), 500, true, false)),
        Follow::Load {
            entry: id(1),
            position_ms: 12_000,
            playing: true
        }
    );
    assert_eq!(
        follow(
            &playing,
            now,
            &local(Some(1), 12_000 - DRIFT_MS, true, false)
        ),
        Follow::InSync
    );
    assert_eq!(
        follow(
            &playing,
            now,
            &local(Some(1), 12_000 + DRIFT_MS + 1, true, false)
        ),
        Follow::Seek {
            position_ms: 12_000
        }
    );
    assert_eq!(
        follow(&playing, now, &local(Some(1), 3_000, false, false)),
        Follow::Resume {
            position_ms: 12_000
        }
    );
    // Waiting for bytes: no seek, no resume, it would only restart the wait.
    assert_eq!(
        follow(&playing, now, &local(Some(1), 3_000, false, true)),
        Follow::InSync
    );
    assert_eq!(
        follow(&playing, now, &local(Some(1), 3_000, true, true)),
        Follow::InSync
    );

    let paused = anchor(Some(1), 10_000, 50_000, false);
    assert_eq!(
        follow(&paused, 90_000, &local(Some(1), 11_000, true, false)),
        Follow::Pause {
            position_ms: 10_000
        }
    );
    assert_eq!(
        follow(&paused, 90_000, &local(Some(1), 10_100, false, false)),
        Follow::InSync
    );
    assert_eq!(
        follow(&paused, 90_000, &local(Some(1), 2_000, false, false)),
        Follow::Seek {
            position_ms: 10_000
        }
    );

    let idle = Anchor::idle(0);
    assert_eq!(
        follow(&idle, 5, &local(Some(1), 0, true, false)),
        Follow::Stop
    );
    assert_eq!(
        follow(&idle, 5, &local(None, 0, false, false)),
        Follow::InSync
    );
}

#[test]
fn an_estimate_before_the_anchor_reads_as_the_anchor() {
    let a = anchor(Some(1), 10_000, 50_000, true);
    assert_eq!(a.position_at(49_000), 10_000);
    assert_eq!(a.position_at(u64::MAX), u64::MAX - 40_000);
    let far = anchor(Some(1), u64::MAX - 5, 0, true);
    assert_eq!(far.position_at(1_000), u64::MAX, "saturates");
}
