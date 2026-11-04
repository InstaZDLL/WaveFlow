//! Changing speed without changing pitch, and transposing, for playback.
//!
//! [`Stretcher`] wraps [Signalsmith Stretch], vendored under
//! `vendor/signalsmith-stretch` (MIT), behind the same append-to-`out` shape as
//! the resampler. It sits after the resampler, so it works at the output's rate
//! on the output's channels, one instance per stream: during a crossfade each
//! track has its own, and the mix comes after.
//!
//! It only ever changes time. A transposition is the resampler's fake source
//! rate, which moves every partial by exactly the same ratio, and the stretcher
//! gives back the time that took ([`TimeShift`]). The library can transpose by
//! itself, but measured it is out by up to 5 cents at 440 Hz and 12 cents at
//! 110 Hz, and by a different amount per note.
//!
//! It adds no delay and no length. The library needs input ahead of what it
//! plays, and answers late by what it holds: left alone, a track starts about
//! 120 ms late and ends that much long, which is a hole in every gapless
//! transition. So the first frames go in as pre-roll, the output's own delay is
//! dropped, and the end is cut at `input / tempo`, to the frame.
//!
//! The phases a phase vocoder moves also move the peaks: a master limited to
//! full scale comes out with peaks near 2.0, and the decoder's last stage would
//! clip them hard. [`PeakLimiter`] takes them down, on this path only.
//!
//! [Signalsmith Stretch]: https://github.com/Signalsmith-Audio/signalsmith-stretch

use std::collections::VecDeque;
use std::ffi::c_int;
use std::ptr::NonNull;

#[repr(C)]
struct Handle {
    _private: [u8; 0],
}

extern "C" {
    fn wf_stretch_new(channels: c_int, block: usize, interval: usize) -> *mut Handle;
    fn wf_stretch_free(handle: *mut Handle);
    fn wf_stretch_reset(handle: *mut Handle) -> bool;
    fn wf_stretch_input_latency(handle: *const Handle) -> usize;
    fn wf_stretch_output_latency(handle: *const Handle) -> usize;
    fn wf_stretch_seek(handle: *mut Handle, input: *const f32, frames: usize, rate: f64) -> bool;
    fn wf_stretch_process(
        handle: *mut Handle,
        input: *const f32,
        input_frames: usize,
        output: *mut f32,
        output_frames: usize,
    ) -> bool;
    fn wf_stretch_flush(handle: *mut Handle, output: *mut f32, frames: usize) -> bool;
}

/// Spectral block. Measured against the library's own 120 ms on speech and
/// music: at 1.5× and 2× the spectrum stays markedly closer to the source, at
/// the same CPU.
const BLOCK_MS: f64 = 80.0;

/// Blocks per interval, the library's own presets' ratio.
const BLOCK_OVERLAP: usize = 4;

/// A transposed fake source rate is rounded to a multiple of this. The ratio of
/// a semitone is irrational, so unrounded the rate shares next to nothing with
/// the output's, and rubato's FFT grows to match: up to a second long, and half
/// of that is delay. 10 Hz leaves it a tenth of that, for less than 0.4 cents an
/// octave under 44.1 kHz, which [`TimeShift::stretch_tempo`] takes out of the
/// clock.
const TRANSPOSED_RATE_STEP: f64 = 10.0;

/// How playback departs from the source's own time and pitch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeShift {
    /// Track time per second of output.
    pub speed: f32,
    /// Whether [`Self::speed`] leaves the pitch alone. Otherwise the pitch
    /// follows it, as on tape: that one is the resampler's fake source rate.
    pub keep_pitch: bool,
    /// Transposition, whichever way the speed is made.
    pub semitones: f32,
}

impl TimeShift {
    /// The source rate the resampler is told, for a true source rate of `src`:
    /// playing it as if it were faster raises the pitch, and takes less time.
    /// With neither a tape speed nor a transposition, `src` itself.
    pub fn resampler_rate(&self, src: u32) -> u32 {
        let tape = if self.keep_pitch { 1.0 } else { self.speed };
        if self.semitones == 0.0 {
            return ((src as f32) * tape).max(1.0) as u32;
        }
        let ratio = 2f64.powf(f64::from(self.semitones) / 12.0);
        let rate = f64::from(src) * f64::from(tape) * ratio;
        ((rate / TRANSPOSED_RATE_STEP).round() * TRANSPOSED_RATE_STEP).max(TRANSPOSED_RATE_STEP)
            as u32
    }

    /// What the stretcher stretches by: the speed asked for, over what the
    /// resampler's rate has already made of it. The two together are always
    /// the speed, and the clock counts on that.
    pub fn stretch_tempo(&self, src: u32) -> f64 {
        f64::from(self.speed) * f64::from(src) / f64::from(self.resampler_rate(src))
    }

    /// Whether a stretcher runs at all. At 1× with no transposition none is
    /// built and the samples go through as if this module did not exist, which
    /// is what keeps a bit-perfect output bit-perfect.
    pub fn stretches(&self) -> bool {
        (self.keep_pitch && (self.speed - 1.0).abs() > f32::EPSILON) || self.semitones != 0.0
    }
}

/// Any source rate: two shifts that come to the same at this one are the same.
const RATE_PROBE: u32 = 48_000;

impl TimeShift {
    /// Whether a stream built for one can play the other as it is. At 1× with
    /// no transposition, keeping the pitch or not is the same audio, and a
    /// rebuild would only cost a drained ring.
    pub fn same_audio(&self, other: &Self) -> bool {
        self.resampler_rate(RATE_PROBE) == other.resampler_rate(RATE_PROBE)
            && self.stretches() == other.stretches()
            && (!self.stretches()
                || self.stretch_tempo(RATE_PROBE) == other.stretch_tempo(RATE_PROBE))
    }
}

/// The library failed to allocate, or was given a channel count it refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StretchError(pub String);

impl std::fmt::Display for StretchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StretchError {}

/// One stream's time-stretch and transposition.
pub struct Stretcher {
    handle: NonNull<Handle>,
    channels: usize,
    tempo: f64,
    /// The first frames of the stream, until there is enough to pre-roll.
    head: Vec<f32>,
    started: bool,
    /// Output frames still to drop: the library's delay.
    to_drop: usize,
    /// Frames of the stream given to [`Self::process`].
    frames_in: u64,
    /// Frames fed to the library since the pre-roll, and asked of it.
    fed: u64,
    asked: u64,
    /// Frames handed to the limiter: the end is cut against this.
    emitted: u64,
    scratch: Vec<f32>,
    limiter: PeakLimiter,
}

// The library instance is plain memory reached only through `&mut self`, and
// it keeps no reference to the thread that made it.
unsafe impl Send for Stretcher {}
unsafe impl Sync for Stretcher {}

impl Stretcher {
    /// A stretcher at the output's `rate`, playing `tempo` times as fast.
    pub fn new(rate: u32, channels: usize, tempo: f64) -> Result<Self, StretchError> {
        let block = ((BLOCK_MS / 1000.0 * f64::from(rate)) as usize).max(BLOCK_OVERLAP);
        let handle = c_int::try_from(channels)
            .ok()
            .filter(|c| *c > 0)
            .and_then(|c| {
                // SAFETY: the shim catches what the constructor throws and
                // answers null, which `NonNull::new` turns into the error below.
                NonNull::new(unsafe { wf_stretch_new(c, block, block / BLOCK_OVERLAP) })
            })
            .ok_or_else(|| StretchError(format!("stretcher init ({channels} channels)")))?;
        let mut this = Self {
            handle,
            channels,
            tempo,
            head: Vec::new(),
            started: false,
            to_drop: 0,
            frames_in: 0,
            fed: 0,
            asked: 0,
            emitted: 0,
            scratch: Vec::new(),
            limiter: PeakLimiter::new(rate, channels),
        };
        this.begin();
        Ok(this)
    }

    /// What this one was built to stretch by.
    pub fn tempo(&self) -> f64 {
        self.tempo
    }

    fn input_latency(&self) -> usize {
        // SAFETY: `handle` lives as long as `self`.
        unsafe { wf_stretch_input_latency(self.handle.as_ptr()) }
    }

    fn output_latency(&self) -> usize {
        // SAFETY: `handle` lives as long as `self`.
        unsafe { wf_stretch_output_latency(self.handle.as_ptr()) }
    }

    /// What every stream start has in common: nothing held, the delay to drop.
    fn begin(&mut self) {
        self.head.clear();
        self.head.reserve(self.input_latency() * self.channels);
        self.started = false;
        self.to_drop = self.output_latency();
        self.frames_in = 0;
        self.fed = 0;
        self.asked = 0;
        self.emitted = 0;
    }

    /// After a seek: forget everything in flight. The stream starts over from
    /// the next frame given, with no delay, as it did from its first.
    pub fn reset(&mut self) -> Result<(), StretchError> {
        // SAFETY: `handle` lives as long as `self`.
        if !unsafe { wf_stretch_reset(self.handle.as_ptr()) } {
            return Err(StretchError("stretcher reset".into()));
        }
        self.begin();
        self.limiter.reset();
        Ok(())
    }

    /// How much the samples are about to be multiplied by once they leave here
    /// (ReplayGain). The limiter takes down what would still be over after it,
    /// and not the peaks of a loud master that gain brings back under.
    pub fn set_output_gain(&mut self, gain: f32) {
        self.limiter.set_output_gain(gain);
    }

    /// Output frames a source frame comes out per frame in.
    fn scale(&self, frames: u64) -> u64 {
        (frames as f64 / self.tempo).round() as u64
    }

    /// Hand `input` to the library, ask for what its length is worth at the
    /// tempo, and pass on what is not the library's own delay.
    fn run(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchError> {
        self.fed += (input.len() / self.channels) as u64;
        let target = self.scale(self.fed);
        let frames = (target - self.asked) as usize;
        self.asked = target;
        self.scratch.resize(frames * self.channels, 0.0);
        // SAFETY: both pointers cover the frame counts given, for the channel
        // count the instance was built with.
        let ok = unsafe {
            wf_stretch_process(
                self.handle.as_ptr(),
                input.as_ptr(),
                input.len() / self.channels,
                self.scratch.as_mut_ptr(),
                frames,
            )
        };
        if !ok {
            return Err(StretchError("stretcher process".into()));
        }
        self.pass_on(frames, out);
        Ok(())
    }

    fn pass_on(&mut self, frames: usize, out: &mut Vec<f32>) {
        let skip = self.to_drop.min(frames);
        self.to_drop -= skip;
        self.limiter.process(
            &self.scratch[skip * self.channels..frames * self.channels],
            out,
        );
        self.emitted += (frames - skip) as u64;
    }

    /// The pre-roll: what the library starts playing from the first frame of.
    fn start(&mut self) -> Result<(), StretchError> {
        self.started = true;
        // SAFETY: `head` holds whole frames, for the channel count the
        // instance was built with.
        let ok = unsafe {
            wf_stretch_seek(
                self.handle.as_ptr(),
                self.head.as_ptr(),
                self.head.len() / self.channels,
                self.tempo,
            )
        };
        if ok {
            Ok(())
        } else {
            Err(StretchError("stretcher pre-roll".into()))
        }
    }

    /// Stretch interleaved `input` and append the result to `out`. The start of
    /// a stream gives nothing back until the pre-roll is full.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchError> {
        debug_assert!(input.len() % self.channels == 0, "input not whole frames");
        self.frames_in += (input.len() / self.channels) as u64;
        if self.started {
            return self.run(input, out);
        }
        let wanted = self.input_latency() * self.channels - self.head.len();
        let taken = wanted.min(input.len());
        self.head.extend_from_slice(&input[..taken]);
        if self.head.len() < self.input_latency() * self.channels {
            return Ok(());
        }
        self.start()?;
        self.run(&input[taken..], out)
    }

    /// At the end of the stream, push out what the library holds and cut the
    /// output at the stream's length, so nothing of the library's own comes out
    /// as silence between two tracks played without a gap.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), StretchError> {
        let total = self.scale(self.frames_in);
        if !self.started {
            // Shorter than the pre-roll: zeros stand for the rest of it.
            let full = self.input_latency() * self.channels;
            self.head.resize(full, 0.0);
            self.start()?;
        }
        let zeros = vec![0.0_f32; self.input_latency() * self.channels];
        let start = out.len();
        self.run(&zeros, out)?;
        let frames = self.output_latency();
        self.scratch.resize(frames * self.channels, 0.0);
        // SAFETY: `scratch` covers `frames` for the instance's channel count.
        if !unsafe { wf_stretch_flush(self.handle.as_ptr(), self.scratch.as_mut_ptr(), frames) } {
            return Err(StretchError("stretcher flush".into()));
        }
        self.pass_on(frames, out);
        self.limiter.flush(out);
        let surplus = self.emitted.saturating_sub(total) as usize * self.channels;
        out.truncate(out.len().saturating_sub(surplus).max(start));
        self.emitted = self.emitted.min(total);
        Ok(())
    }
}

impl Drop for Stretcher {
    fn drop(&mut self) {
        // SAFETY: `handle` came from `wf_stretch_new` and is freed only here.
        unsafe { wf_stretch_free(self.handle.as_ptr()) };
    }
}

/// How far ahead of a peak the gain starts going down.
const LIMITER_LOOKAHEAD_MS: f64 = 2.0;

/// How fast the gain comes back after a peak.
const LIMITER_RELEASE_MS: f64 = 80.0;

/// Where a peak is held: under the clamp that ends the decoder's chain, which
/// then has nothing to do.
const LIMITER_CEILING: f32 = 0.98;

/// A look-ahead peak limiter, one gain for all channels so the image stays
/// where it is. The gain a peak needs is held for the look-ahead, eased back
/// up over the release, and averaged over the look-ahead again: by the time the
/// peak comes out of the delay, every value in that average is at or under what
/// it needs, so it cannot overshoot, and the gain has moved without a step.
struct PeakLimiter {
    channels: usize,
    length: usize,
    /// The frames waiting, interleaved, oldest at `cursor`.
    delay: Vec<f32>,
    /// How many frames `delay` holds.
    held: usize,
    cursor: usize,
    /// Falling gains of the last `length` frames: the front is their minimum.
    minima: VecDeque<(u64, f32)>,
    frame: u64,
    eased: f32,
    release: f32,
    /// The last `length` eased gains, and their sum.
    averaged: Vec<f32>,
    sum: f64,
    output_gain: f32,
}

impl PeakLimiter {
    fn new(rate: u32, channels: usize) -> Self {
        let length = ((LIMITER_LOOKAHEAD_MS / 1000.0 * f64::from(rate)) as usize).max(1);
        let release = 1.0 - (-1000.0 / (LIMITER_RELEASE_MS * f64::from(rate))).exp() as f32;
        Self {
            channels,
            length,
            delay: vec![0.0; length * channels],
            held: 0,
            cursor: 0,
            minima: VecDeque::with_capacity(length + 1),
            frame: 0,
            eased: 1.0,
            release,
            averaged: vec![1.0; length],
            sum: length as f64,
            output_gain: 1.0,
        }
    }

    fn reset(&mut self) {
        self.held = 0;
        self.cursor = 0;
        self.minima.clear();
        self.frame = 0;
        self.eased = 1.0;
        self.averaged.fill(1.0);
        self.sum = self.length as f64;
    }

    fn set_output_gain(&mut self, gain: f32) {
        if gain.is_finite() && gain > 0.0 {
            self.output_gain = gain;
        }
    }

    fn step(&mut self, frame: &[f32], out: &mut Vec<f32>) {
        let peak = frame.iter().fold(0.0_f32, |p, s| p.max(s.abs())) * self.output_gain;
        let needed = if peak > LIMITER_CEILING {
            LIMITER_CEILING / peak
        } else {
            1.0
        };
        while self.minima.back().is_some_and(|(_, g)| *g >= needed) {
            self.minima.pop_back();
        }
        self.minima.push_back((self.frame, needed));
        let oldest_kept = (self.frame + 1).saturating_sub(self.length as u64);
        while self.minima.front().is_some_and(|(at, _)| *at < oldest_kept) {
            self.minima.pop_front();
        }
        let held = self.minima.front().map_or(1.0, |(_, g)| *g);
        self.eased = held.min(self.eased + (1.0 - self.eased) * self.release);
        let slot = (self.frame % self.length as u64) as usize;
        self.sum += f64::from(self.eased) - f64::from(self.averaged[slot]);
        self.averaged[slot] = self.eased;
        // Keeps the running sum from drifting, at no cost worth counting.
        if slot == 0 {
            self.sum = self.averaged.iter().map(|g| f64::from(*g)).sum();
        }
        self.frame += 1;

        let at = (self.cursor + self.held) % self.length * self.channels;
        self.delay[at..at + self.channels].copy_from_slice(frame);
        self.held += 1;
        if self.held == self.length {
            let gain = (self.sum / self.length as f64) as f32;
            let at = self.cursor * self.channels;
            out.extend(self.delay[at..at + self.channels].iter().map(|s| s * gain));
            self.cursor = (self.cursor + 1) % self.length;
            self.held -= 1;
        }
    }

    fn process(&mut self, frames: &[f32], out: &mut Vec<f32>) {
        for frame in frames.chunks_exact(self.channels) {
            self.step(frame, out);
        }
    }

    /// Push out the frames still in the delay.
    fn flush(&mut self, out: &mut Vec<f32>) {
        let silence = vec![0.0_f32; self.channels];
        let mut left = self.held;
        while left > 0 {
            let before = out.len();
            self.step(&silence, out);
            if out.len() > before {
                left -= 1;
            }
        }
        self.reset();
    }
}

#[cfg(test)]
mod tests;
