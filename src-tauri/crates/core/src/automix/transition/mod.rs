//! The pieces a transition is built from, and the planner that picks one.
//!
//! [`super::plan`] places a plain crossfade on the beats. This module goes
//! further: a transition is a [`template::Template`] — a combination of
//! bricks — laid on a pair of points the planner found in both tracks
//! ([`points`]), and the [`planner`] scores every combination it can lay
//! out, keeps the best, or decides not to mix at all when none is good
//! enough, in which case the player makes the transition it makes today.
//!
//! The bricks:
//!
//! - [`envelope`]: how a gain goes from one value to another over a part of
//!   the transition;
//! - [`bands`]: a signal split into bands that add back to it exactly, each
//!   with its own envelope, at cut-offs given as parameters;
//! - [`fx`]: what can be done to the outgoing track on the way out — an
//!   echo on the beat, a loop of its last beats, a filter sweep;
//! - [`level`]: the gain that keeps the overall power where it was while
//!   both tracks sound, and a last-resort limiter that counts what it
//!   touches;
//! - [`tempo`]: a glide of either track's tempo, as a speed over its own
//!   time, with the output time it takes;
//! - [`points`]: where the outgoing track can be left (where its music
//!   stops, a break before that) and where the incoming one can be entered
//!   (its start, or a phrase past its intro), from the level of each beat.
//!
//! Nothing here holds a number taken from one transition: cut-offs, lengths,
//! which side is stretched and where the bass changes hands are fields of a
//! template. [`template::band_swap`] carries one reference setting (AM-001,
//! documented there) as a set of values.
//!
//! **Real time.** Everything that runs per sample ([`bands::BandSplit`],
//! [`fx::Echo`], [`fx::Sweep`], [`level::Limiter`]) allocates when it is
//! built and never after, holds no lock and works on any channel count, the
//! same way on every channel. It is meant for the decoder thread; the audio
//! callback stays as it is.
//!
//! **Bit-perfect.** A future integration must leave every stream untouched
//! when bit-perfect playback is asked for: no template, no stretch, no gain.

pub mod bands;
pub mod envelope;
pub mod fx;
pub mod level;
pub mod planner;
pub mod points;
pub mod template;
pub mod tempo;

#[cfg(test)]
mod tests;
