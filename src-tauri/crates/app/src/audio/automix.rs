//! AutoMix's half of [`super::state::SharedPlayback`]: the switch, and the
//! two hints the decoder thread reads.
//!
//! The decoder cannot read the database, so it asks (`AnalyticsMsg::AutoMixCue`
//! and `PrefetchNext`) and is answered here. Every field is an atomic: the
//! decoder reads them between packets, never in the audio callback.
//!
//! - **The cue**: for the track playing, when to ask for the next one. A
//!   plan starts its fade up to a crossfade length before the outgoing
//!   track's last beat, which can be well before its end; asked at the
//!   usual moment, the next track would arrive too late for it.
//! - **The plan**: for one pair of tracks, when the incoming one starts
//!   (the fade with it) and when the fade ends, in the outgoing track's
//!   time. It rides in the `SetNextTrack` it belongs to and the decoder
//!   stores it with the stream, keyed on both track ids so a plan for
//!   another pair is never applied.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

pub struct AutoMixShared {
    /// The switch, `profile_setting['audio.automix']`, default off. Only
    /// acts while a crossfade length is set.
    pub enabled: AtomicBool,
    cue_track: AtomicI64,
    cue_prefetch_at_ms: AtomicU64,
    plan_out: AtomicI64,
    plan_in: AtomicI64,
    fade_start_us: AtomicU64,
    fade_end_us: AtomicU64,
}

impl Default for AutoMixShared {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoMixShared {
    pub const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            cue_track: AtomicI64::new(0),
            cue_prefetch_at_ms: AtomicU64::new(0),
            plan_out: AtomicI64::new(0),
            plan_in: AtomicI64::new(0),
            fade_start_us: AtomicU64::new(0),
            fade_end_us: AtomicU64::new(0),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Ask for the track after `track_id` once the decoder reaches
    /// `at_ms` of it.
    pub fn set_cue(&self, track_id: i64, at_ms: u64) {
        self.cue_prefetch_at_ms.store(at_ms, Ordering::Relaxed);
        self.cue_track.store(track_id, Ordering::Release);
    }

    /// Leave no cue for `track_id`: it is asked about at the usual moment.
    /// A cue left by an earlier play of the same id (another profile, an
    /// analysis since) must not outlive it.
    pub fn clear_cue(&self, track_id: i64) {
        let _ = self
            .cue_track
            .compare_exchange(track_id, 0, Ordering::AcqRel, Ordering::Relaxed);
    }

    /// When to ask for the track after `track_id`, if a cue was left for
    /// that track.
    pub fn prefetch_at_ms(&self, track_id: i64) -> Option<u64> {
        (self.cue_track.load(Ordering::Acquire) == track_id)
            .then(|| self.cue_prefetch_at_ms.load(Ordering::Relaxed))
    }

    /// Leave the plan for `out_id` → `in_id`, or clear any plan with
    /// `None`. The times are the outgoing track's, in microseconds.
    pub fn set_plan(&self, out_id: i64, in_id: i64, fade: Option<(u64, u64)>) {
        // The ids go last: a reader that matches them sees the times
        // written with them.
        self.plan_in.store(0, Ordering::Release);
        let Some((start, end)) = fade else {
            self.plan_out.store(0, Ordering::Release);
            return;
        };
        self.fade_start_us.store(start, Ordering::Relaxed);
        self.fade_end_us.store(end, Ordering::Relaxed);
        self.plan_out.store(out_id, Ordering::Release);
        self.plan_in.store(in_id, Ordering::Release);
    }

    /// The plan for `out_id` → `in_id`: `(fade_start_us, fade_end_us)`.
    pub fn plan_for(&self, out_id: i64, in_id: i64) -> Option<(u64, u64)> {
        if self.plan_in.load(Ordering::Acquire) != in_id
            || self.plan_out.load(Ordering::Acquire) != out_id
        {
            return None;
        }
        Some((
            self.fade_start_us.load(Ordering::Relaxed),
            self.fade_end_us.load(Ordering::Relaxed),
        ))
    }

    /// Drop the plan: the transition it was for is no longer coming.
    pub fn clear_plan(&self) {
        self.set_plan(0, 0, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_is_only_for_its_own_pair() {
        let a = AutoMixShared::new();
        assert_eq!(a.plan_for(1, 2), None);
        a.set_plan(1, 2, Some((10, 20)));
        assert_eq!(a.plan_for(1, 2), Some((10, 20)));
        assert_eq!(a.plan_for(1, 3), None);
        assert_eq!(a.plan_for(4, 2), None);
        a.set_plan(1, 2, None);
        assert_eq!(a.plan_for(1, 2), None);
    }

    #[test]
    fn a_cue_is_only_for_its_own_track() {
        let a = AutoMixShared::new();
        assert_eq!(a.prefetch_at_ms(7), None);
        a.set_cue(7, 180_000);
        assert_eq!(a.prefetch_at_ms(7), Some(180_000));
        assert_eq!(a.prefetch_at_ms(8), None);
        a.clear_cue(8);
        assert_eq!(a.prefetch_at_ms(7), Some(180_000));
        a.clear_cue(7);
        assert_eq!(a.prefetch_at_ms(7), None);
    }
}
