//! Process-wide offline-mode flag.
//!
//! When `true`, every code path that would otherwise hit Last.fm,
//! Deezer or LRCLIB short-circuits and returns the locally cached
//! result (or an empty payload). Stored as a global atomic because
//! offline is a network-stack concern, not a per-profile preference —
//! switching profiles must not silently re-enable network calls.
//!
//! Hydrated from `app_setting['network.offline_mode']` during
//! [`crate::state::AppState::init`] and flipped by
//! [`crate::commands::offline::set_offline_mode`] when the user toggles
//! the switch in Settings.

use std::sync::atomic::{AtomicBool, Ordering};

static IS_OFFLINE: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn is_offline() -> bool {
    IS_OFFLINE.load(Ordering::Acquire)
}

#[inline]
pub fn set(value: bool) {
    IS_OFFLINE.store(value, Ordering::Release);
}
