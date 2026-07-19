//! Shared state between the decoder thread and the real-time cpal callback.
//!
//! Every field is an atomic because the cpal audio callback MUST NOT take
//! any locks. The decoder thread and tauri command handlers write, the
//! audio callback and UI reads.

use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering,
};

/// High-level player lifecycle. Stored as `AtomicU8` — see [`PlayerState::from_u8`]
/// for the inverse of `as u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PlayerState {
    Idle = 0,
    Loading = 1,
    Playing = 2,
    Paused = 3,
    Ended = 4,
}

impl PlayerState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Loading,
            2 => Self::Playing,
            3 => Self::Paused,
            4 => Self::Ended,
            _ => Self::Idle,
        }
    }

    /// Short string the frontend uses to discriminate states in events.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Ended => "ended",
        }
    }
}

/// Lock-free state block shared between threads.
///
/// Layout invariants:
/// - `samples_played` is advanced by **whichever output is driving the
///   ring** — the cpal callback, or the WASAPI Exclusive render thread
///   when that mode is engaged. Both count only samples actually popped,
///   never the silence written on an underrun. Only the decoder thread
///   resets it (on load/seek), and a reset must bump `seek_generation` to
///   invalidate any in-flight consumer reads.
///
///   This is the single source of truth for playback position:
///   `current_position_ms` and `session_listened_ms` both derive from it,
///   so an output path that forgets to advance it leaves the progress bar
///   and lyrics frozen and credits every play as 0 ms. That is exactly
///   what the exclusive path did until #405 — this comment used to say
///   "only by the cpal callback", which was true when written and stopped
///   being true when the second output landed.
/// - `sample_rate` / `channels` are written once when the output stream
///   opens and never mutated again.
/// - `volume_bits` holds an `f32` in `[0.0, 1.0]` via `to_bits` / `from_bits`.
/// - `base_offset_ms` holds the playback position at the last seek target or
///   track load start, so `current_position_ms()` can add it to the delta
///   derived from `samples_played`.
pub struct SharedPlayback {
    pub state: AtomicU8,
    pub samples_played: AtomicU64,
    pub sample_rate: AtomicU32,
    pub channels: AtomicU16,
    pub volume_bits: AtomicU32,
    pub seek_generation: AtomicU64,
    pub base_offset_ms: AtomicU64,
    /// ID of the track currently loaded in the decoder (0 = none).
    /// Written by the decoder thread at `LoadAndPlay` time, read by
    /// the shutdown hook so it can persist the resume point.
    pub current_track_id: AtomicI64,
    /// When `true`, the cpal output callback writes silence instead of
    /// draining the SPSC ring — making pause audibly instant even when
    /// the decoder has ~1 s of pre-buffered samples. The decoder
    /// thread flips this alongside `state` in `drain_commands`.
    pub paused_output: AtomicBool,
    /// When `true`, the cpal output callback pops from the ring AND
    /// writes silence. Used by the decoder thread to quickly empty
    /// the ring during a track switch without letting any of the
    /// old track's samples reach the device. Distinct from
    /// `paused_output`: the latter preserves the ring for an
    /// instant resume; this one intentionally drops it.
    pub drain_silent: AtomicBool,
    /// When `true`, the cpal callback applies a −3 dB gain reduction
    /// (× 0.707) to all samples to prevent clipping on loud tracks.
    /// Toggled from the Settings "Normalize volume" switch.
    pub normalize_enabled: AtomicBool,
    /// When `true`, the cpal callback averages L+R channels so that
    /// every output channel receives the same mono signal. Useful
    /// for single-speaker setups or users with hearing impairment.
    pub mono_enabled: AtomicBool,
    /// Crossfade duration in milliseconds (0 = disabled). The decoder
    /// thread reads this each packet to decide when to prefetch the
    /// next track and when to start mixing.
    pub crossfade_ms: AtomicU32,
    /// When `true`, the decoder thread multiplies decoded samples by
    /// each track's stored ReplayGain factor (computed by `analysis.rs`
    /// and read from `track_analysis.replay_gain_db` at load time).
    /// Toggled from the Settings "Apply ReplayGain" switch.
    pub replaygain_enabled: AtomicBool,
    /// When `true`, the decoder pre-fetches the next queued track
    /// ~500 ms before the current one ends and swaps to it the
    /// instant primary EOFs — no analytics → LoadAndPlay round trip,
    /// no decoder spin-up gap. Distinct from `crossfade_ms`: gapless
    /// is a sample-accurate baton hand-off (no fade), crossfade is a
    /// timed equal-power mix. When `crossfade_ms > 0` crossfade wins
    /// (the fade implicitly subsumes the gap, no point in both).
    pub gapless_enabled: AtomicBool,
    /// 6-band peaking equaliser. Bands and bypass are atomics on the
    /// shared struct (so the UI can mutate them without bouncing
    /// through a command queue); the per-channel filter state lives
    /// on the decoder thread inside `EqProcessor`.
    pub eq: super::eq::EqShared,
    /// When `true`, the analytics worker skips the auto-advance step
    /// after the current track ends naturally. Used by the sleep
    /// timer's "end of current track" mode: the frontend arms this
    /// flag, the timer fires its fade + pause when the track ends,
    /// and the queue cursor stays put so the user can resume from
    /// the same spot. Auto-clears after consumption (one-shot).
    pub pause_after_current_track: AtomicBool,
    /// A-B repeat: when both `loop_a_ms` and `loop_b_ms` are non-zero
    /// AND `loop_b_ms > loop_a_ms`, the decoder seeks back to `A` once
    /// playback reaches `B`. Both are unsigned ms inside the current
    /// track; the loop is cleared by the user (or implicitly when the
    /// track changes — see the LoadAndPlay handler).
    pub loop_a_ms: AtomicU64,
    pub loop_b_ms: AtomicU64,
    /// When `true`, the decoder thread feeds the post-EQ stream to
    /// the spectrum analyzer and emits `player:spectrum` frames at
    /// ~30 Hz. When `false`, the analyzer short-circuits to a no-op
    /// so the cost of FFT + event encoding is zero. Persisted in
    /// `profile_setting['ui.visualizer']`.
    pub visualizer_enabled: AtomicBool,
    /// When `true` AND a crossfade window is configured, the decoder
    /// suppresses the fade between two tracks belonging to the same
    /// album — concept records / live sets hand off naturally instead
    /// of getting smeared by an equal-power mix. The same-album
    /// decision is computed by the analytics worker on every
    /// PrefetchNext and stashed in `pending_next_same_album` for the
    /// decoder to consult at mix-decision time. Persisted in
    /// `profile_setting['audio.smart_crossfade']`, default OFF —
    /// it's an opinionated behaviour change so users opt in.
    pub smart_crossfade_enabled: AtomicBool,
    /// One-shot hint set by the analytics worker right before
    /// dispatching `SetNextTrack`: `true` when the upcoming track
    /// shares an album_id with the currently-playing track. Cleared
    /// implicitly when the next track is consumed (LoadAndPlay /
    /// pending_next swap) so a stale value can't bleed into the
    /// transition after.
    pub pending_next_same_album: AtomicBool,
    /// When `true` AND a crossfade window is configured, the analytics
    /// worker scales the upcoming fade duration by the tempo gap
    /// between the current and next tracks. Similar BPM → keep the
    /// full window (mixes blend cleanly); large BPM gap → shrink the
    /// window so the transition snaps before the rhythms can clash.
    /// Persisted in `profile_setting['audio.dynamic_crossfade']`,
    /// default OFF — opt-in like smart crossfade.
    pub dynamic_crossfade_enabled: AtomicBool,
    /// One-shot crossfade override in ms, set by the analytics worker
    /// right before `SetNextTrack` when dynamic crossfade is armed.
    /// `0` = no override (decoder falls back to `crossfade_ms`).
    /// Cleared by the decoder the instant the mix actually starts so
    /// it can't bleed into the transition after.
    pub pending_next_crossfade_ms: AtomicU32,
    /// Playback speed multiplier, stored as `f32` bits. `1.0` is
    /// normal; `<1.0` is slower / lower-pitched, `>1.0` is faster /
    /// higher-pitched. The decoder achieves this by feeding rubato a
    /// fake source rate of `actual_rate * speed`, so 1 source sample
    /// of audio produces fewer output samples at the device rate.
    /// Clamped to `[0.5, 2.0]` on every write. Pitch is NOT
    /// preserved — proper time-stretching would need a phase vocoder.
    pub playback_speed_bits: AtomicU32,
    /// Set by `player_set_speed` to tell the decoder thread to rebuild
    /// every stream's resampler at the new speed on the next decode
    /// cycle. Cleared by the decoder once consumed.
    pub speed_dirty: AtomicBool,
    /// FIR tap count for the DSD → PCM converter. Read by the decoder
    /// when it opens a `.dsf` / `.dff` stream (not in the hot path).
    /// Default 256 (the historical fixed value); the user can raise it
    /// to 1024 / 2048 for a sharper anti-alias filter on capable
    /// hardware. Only affects DSD playback — symphonia formats ignore
    /// it. Takes effect on the next track open.
    pub dsd_taps: AtomicU32,
}

impl SharedPlayback {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(PlayerState::Idle as u8),
            samples_played: AtomicU64::new(0),
            sample_rate: AtomicU32::new(0),
            channels: AtomicU16::new(0),
            volume_bits: AtomicU32::new(1.0_f32.to_bits()),
            seek_generation: AtomicU64::new(0),
            base_offset_ms: AtomicU64::new(0),
            current_track_id: AtomicI64::new(0),
            paused_output: AtomicBool::new(false),
            drain_silent: AtomicBool::new(false),
            normalize_enabled: AtomicBool::new(false),
            mono_enabled: AtomicBool::new(false),
            crossfade_ms: AtomicU32::new(0),
            replaygain_enabled: AtomicBool::new(false),
            gapless_enabled: AtomicBool::new(true),
            eq: super::eq::EqShared::new(),
            pause_after_current_track: AtomicBool::new(false),
            loop_a_ms: AtomicU64::new(0),
            loop_b_ms: AtomicU64::new(0),
            visualizer_enabled: AtomicBool::new(false),
            smart_crossfade_enabled: AtomicBool::new(false),
            pending_next_same_album: AtomicBool::new(false),
            dynamic_crossfade_enabled: AtomicBool::new(false),
            pending_next_crossfade_ms: AtomicU32::new(0),
            playback_speed_bits: AtomicU32::new(1.0_f32.to_bits()),
            speed_dirty: AtomicBool::new(false),
            dsd_taps: AtomicU32::new(256),
        }
    }

    /// Current playback speed multiplier, clamped to `[0.5, 2.0]`.
    pub fn playback_speed(&self) -> f32 {
        f32::from_bits(self.playback_speed_bits.load(Ordering::Relaxed))
    }

    /// Write a new playback speed and flag the decoder to rebuild its
    /// resampler on the next decode cycle. Values outside `[0.5, 2.0]`
    /// are clamped to the supported range — out-of-range speeds make
    /// rubato unstable and aren't useful for music playback.
    ///
    /// Snapshots the current track position **at the old speed** and
    /// rebases `samples_played` / `base_offset_ms` against it before
    /// flipping the speed atomic. Without this, the next read of
    /// `current_position_ms` would re-scale the existing
    /// `samples_played` counter by the new speed and the progress bar
    /// would jump backwards (slowing down) or forwards (speeding up).
    pub fn set_playback_speed(&self, speed: f32) {
        let clamped = speed.clamp(0.5, 2.0);
        let pos = self.current_position_ms();
        self.samples_played.store(0, Ordering::Relaxed);
        self.base_offset_ms.store(pos, Ordering::Release);
        self.seek_generation.fetch_add(1, Ordering::Release);
        self.playback_speed_bits
            .store(clamped.to_bits(), Ordering::Release);
        self.speed_dirty.store(true, Ordering::Release);
    }

    /// True when an A-B loop is currently armed (A < B and both set).
    pub fn ab_loop_armed(&self) -> Option<(u64, u64)> {
        let a = self.loop_a_ms.load(Ordering::Relaxed);
        let b = self.loop_b_ms.load(Ordering::Relaxed);
        if b > a && b > 0 {
            Some((a, b))
        } else {
            None
        }
    }

    pub fn clear_ab_loop(&self) {
        self.loop_a_ms.store(0, Ordering::Relaxed);
        self.loop_b_ms.store(0, Ordering::Relaxed);
    }

    pub fn state(&self) -> PlayerState {
        PlayerState::from_u8(self.state.load(Ordering::Acquire))
    }

    pub fn set_state(&self, state: PlayerState) {
        self.state.store(state as u8, Ordering::Release);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, v: f32) {
        let clamped = v.clamp(0.0, 1.0);
        self.volume_bits.store(clamped.to_bits(), Ordering::Relaxed);
    }

    /// Current position **inside the track** in ms, derived from the
    /// callback-advanced sample counter plus the base offset written
    /// on load / seek. Use this to drive the progress bar and seek
    /// display — the user wants to know "where am I in the song",
    /// not "how long has this session been running".
    ///
    /// At non-1× playback speeds, the decoder feeds rubato a fake
    /// source rate of `actual_rate * speed` so each cpal output sample
    /// represents `speed` source samples of audio. We scale the
    /// callback-derived delta by `speed` so the progress bar advances
    /// in track-time, not wall-clock-time.
    pub fn current_position_ms(&self) -> u64 {
        let sr = self.sample_rate.load(Ordering::Relaxed).max(1) as u64;
        let ch = self.channels.load(Ordering::Relaxed).max(1) as u64;
        let played = self.samples_played.load(Ordering::Relaxed);
        let wall_delta_ms = (played * 1000) / (sr * ch);
        let speed = self.playback_speed();
        let track_delta_ms = (wall_delta_ms as f32 * speed) as u64;
        self.base_offset_ms.load(Ordering::Relaxed) + track_delta_ms
    }

    /// Number of ms actually heard **in the current session** — i.e.
    /// since the last `LoadAndPlay` reset `samples_played` to zero.
    /// Distinct from [`Self::current_position_ms`] which adds the
    /// `base_offset_ms` for resumes / seeks. Analytics uses this one
    /// so that resuming a track at 2:30 and listening for 3 s counts
    /// as a 3 s listen (not a 2:33 listen), which matters for the
    /// "Recently played" 15 s credit threshold.
    ///
    /// Scaled by playback speed for the same reason as
    /// [`Self::current_position_ms`] — at 2× speed a 30 s listen
    /// covers 60 s of track, and the credit threshold should fire
    /// based on track-time covered.
    pub fn session_listened_ms(&self) -> u64 {
        let sr = self.sample_rate.load(Ordering::Relaxed).max(1) as u64;
        let ch = self.channels.load(Ordering::Relaxed).max(1) as u64;
        let played = self.samples_played.load(Ordering::Relaxed);
        let wall_ms = (played * 1000) / (sr * ch);
        let speed = self.playback_speed();
        (wall_ms as f32 * speed) as u64
    }
}

impl Default for SharedPlayback {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
