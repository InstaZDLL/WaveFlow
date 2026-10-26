//! Decoding for playback, shared by the desktop player and anything else
//! that plays a file through WaveFlow's pipeline: open a track, decode it
//! to interleaved `f32`, resample it to the output's rate, fit its channels
//! to the output's, and weigh its ReplayGain.
//!
//! What stays with the player is everything real-time or app-bound: the
//! decoder thread and its commands, crossfades, the output device, the
//! equalizer, events. Behind the `playback` feature, so a build that
//! never plays audio (the server) does not compile the resampler.

pub mod channels;
pub mod hls;
pub mod read_ahead;
pub mod replay_gain;
pub mod resampler;
pub mod stream;
pub mod timeshift;
