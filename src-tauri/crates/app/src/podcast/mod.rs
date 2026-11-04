//! Podcasts, standalone and opt-in ([RFC-009]).
//!
//! The pure half, what a feed means, is [`waveflow_core::podcast`]. What lives
//! here needs the app: the network ([`fetch`]), the profile's database
//! ([`store`]), the passes that join the two ([`refresh`]), the timer
//! ([`schedule`]) and the episodes kept on disk ([`download`]). The Tauri commands
//! are `commands::podcast`.
//!
//! A podcast is outbound traffic in an app that otherwise needs none, so with
//! `podcasts.enabled` off nothing here contacts anything and no timer runs. What
//! was stored stays readable.
//!
//! An episode is not a track: it has no `track` row, and nothing here touches
//! the library, the statistics or the scrobbling.
//!
//! [RFC-009]: ../../../../docs/rfcs/RFC-009-podcasts.md

pub(crate) mod download;
pub(crate) mod fetch;
pub(crate) mod refresh;
pub(crate) mod schedule;
pub(crate) mod store;

#[cfg(test)]
mod tests;

/// Emitted when a channel or its episodes changed. Carries `channel_id`, or
/// `null` when several may have.
pub const PODCAST_UPDATED: &str = "podcast:updated";
