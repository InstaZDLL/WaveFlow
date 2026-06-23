//! Shared audio-engine event payloads.
//!
//! Some events are emitted from more than one place in the engine —
//! `player:radio-metadata`, for instance, fires once up front from the
//! [`super::decoder`] loop (static station name) and again from
//! [`super::http_source`] every time the live ICY `StreamTitle`
//! changes. The wire shape MUST stay byte-identical to the frontend's
//! `RadioMetadataPayload` listener, so it lives here once rather than
//! being copy-pasted into each emitter (which risks silent drift if one
//! copy gains a field or a serde rename the other doesn't).

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// Tauri event name for live-stream "now playing" updates.
pub const EVENT_RADIO_METADATA: &str = "player:radio-metadata";

/// Wire shape of [`EVENT_RADIO_METADATA`]. `track_id` is the negative
/// sentinel `player_play_url` mints for URL streams. snake_case (no
/// `rename_all`) to match the frontend type verbatim.
///
/// Two layers travel together:
///
/// - **Now playing** (`title` / `artist` / `artwork_url`) — the live
///   song from ICY metadata. Equals the station on the first emit,
///   then tracks each `StreamTitle` change.
/// - **Station identity** (`station_*`) — the *stable* stream so a
///   favorite can be built (and the PlayerBar / mini-player can keep
///   the station name + cover) even after the now-playing line has been
///   overwritten with a song title. `station_url` is the raw stream URL
///   (the favorite id is `url:<station_url>`).
#[derive(Serialize, Clone)]
pub struct RadioMetadataPayload {
    pub track_id: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artwork_url: Option<String>,
    pub station_url: Option<String>,
    pub station_name: Option<String>,
    pub station_artist: Option<String>,
    pub station_artwork: Option<String>,
}

/// Last emitted radio metadata, kept process-wide so a webview that
/// mounts mid-stream (the mini-player opened after a station started)
/// can hydrate via `get_current_radio_metadata` — `player_get_state`
/// can't carry it because radio has no library row. Process-wide (not
/// per-profile / per-`AppState`) because there is exactly one audio
/// engine, mirroring [`crate::offline`]. Cleared when a non-radio track
/// plays or playback stops.
static LAST_RADIO_METADATA: Mutex<Option<RadioMetadataPayload>> = Mutex::new(None);

/// Snapshot the last radio metadata (for the hydration command). Poison
/// is recovered rather than panicking — a dropped snapshot is cosmetic.
pub fn last_radio_metadata() -> Option<RadioMetadataPayload> {
    LAST_RADIO_METADATA
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Clear the stored radio metadata — called when a library track starts
/// or playback stops so a later mini-player hydration doesn't resurrect
/// a stale station.
pub fn clear_radio_metadata() {
    *LAST_RADIO_METADATA.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Emit `player:radio-metadata` AND stash it for late-mounting webviews.
/// Errors are swallowed — a dropped metadata frame is cosmetic (the
/// PlayerBar keeps the prior title) and never worth interrupting
/// playback for.
pub fn emit_radio_metadata(app: &AppHandle, payload: RadioMetadataPayload) {
    *LAST_RADIO_METADATA.lock().unwrap_or_else(|e| e.into_inner()) = Some(payload.clone());
    let _ = app.emit(EVENT_RADIO_METADATA, payload);
}
