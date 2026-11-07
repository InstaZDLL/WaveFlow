//! Process-wide offline-mode flag.
//!
//! When `true`, every code path that would otherwise reach an online
//! service — metadata and artwork, lyrics, scrobbling, plugins, radio,
//! podcasts, the remote server and network shares, the automatic update
//! check — short-circuits and returns the locally cached result (or an
//! empty payload, or a refusal). Discord Rich Presence (a local pipe)
//! and the DLNA / MPD servers (the local network) have their own
//! switches and do not read it. Stored as a global atomic because
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
    #[cfg(test)]
    if let Some(value) = THIS_THREAD.get() {
        return value;
    }
    IS_OFFLINE.load(Ordering::Acquire)
}

#[inline]
pub fn set(value: bool) {
    IS_OFFLINE.store(value, Ordering::Release);
}

// The flag is process-wide and tests run in parallel: a test flipping it would
// fail any other that happens to be talking to a local server at that moment.
// So a test answers for its own thread, which is also the only one a
// `#[tokio::test]` runs on.
#[cfg(test)]
thread_local! {
    static THIS_THREAD: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// What [`is_offline`] answers on this thread, until the handle drops.
#[cfg(test)]
pub(crate) fn scoped_to_this_thread(value: bool) -> ThisThread {
    THIS_THREAD.set(Some(value));
    ThisThread
}

/// Changes the answer inside a [`scoped_to_this_thread`], the way the toggle in
/// Settings would change it while a request is in flight.
#[cfg(all(test, feature = "sync_v2"))]
pub(crate) fn flip_on_this_thread(value: bool) {
    THIS_THREAD.set(Some(value));
}

/// Reset by drop: a panic must not leave the thread's next test offline.
#[cfg(test)]
pub(crate) struct ThisThread;

#[cfg(test)]
impl Drop for ThisThread {
    fn drop(&mut self) {
        THIS_THREAD.set(None);
    }
}
