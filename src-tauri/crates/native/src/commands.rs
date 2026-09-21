//! UI-independent event publication used by the unchanged player actions.
pub mod player {
    pub(crate) use crate::playback_gain::fetch_replay_gain;
    use crate::{
        host::{AppHandle, Emitter, Manager},
        paths::AppPaths,
        queue::QueueTrack,
        state::AppState,
    };

    pub(crate) fn emit_track_changed(
        app: &AppHandle,
        _: &AppPaths,
        track: &QueueTrack,
        _: Option<i64>,
    ) {
        app.state::<AppState>().remote_playback.clear();
        let _ = app.emit("player:track-changed", track.clone());
    }
    pub(crate) fn emit_queue_changed(app: &AppHandle) {
        let _ = app.emit("player:queue-changed", ());
    }
}

pub mod scan {
    /// The native adapter does not start a scanner yet. The existing database
    /// backfill still observes this boundary, ready for the shared scan service.
    pub fn scan_in_flight() -> bool {
        false
    }
}
