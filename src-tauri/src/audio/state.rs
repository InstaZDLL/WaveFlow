//! Shared state between the decoder thread and the real-time cpal callback.
//!
//! Every field is an atomic because the cpal audio callback MUST NOT take
//! any locks. The decoder thread and tauri command handlers write, the
//! audio callback and UI reads.

use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

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
/// - `samples_played` is advanced only by the cpal callback, only reset by
///   the decoder thread (on load/seek). Reset must bump `seek_generation`
///   to invalidate any in-flight consumer reads.
/// - `sample_rate` / `channels` are written once when the cpal stream opens
///   and never mutated again.
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
        }
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
        self.volume_bits
            .store(clamped.to_bits(), Ordering::Relaxed);
    }

    /// Current wall-clock position in ms, derived from the callback-advanced
    /// sample counter + the base offset written on load / seek. Returns 0
    /// before the stream opens (sample_rate / channels are still 0).
    pub fn current_position_ms(&self) -> u64 {
        let sr = self.sample_rate.load(Ordering::Relaxed).max(1) as u64;
        let ch = self.channels.load(Ordering::Relaxed).max(1) as u64;
        let played = self.samples_played.load(Ordering::Relaxed);
        let delta_ms = (played * 1000) / (sr * ch);
        self.base_offset_ms.load(Ordering::Relaxed) + delta_ms
    }
}

impl Default for SharedPlayback {
    fn default() -> Self {
        Self::new()
    }
}
