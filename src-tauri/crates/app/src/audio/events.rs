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

use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// Tauri event name for live-stream "now playing" updates.
pub const EVENT_RADIO_METADATA: &str = "player:radio-metadata";

/// Wire shape of [`EVENT_RADIO_METADATA`]. `track_id` is the negative
/// sentinel `player_play_url` mints for URL streams. snake_case (no
/// `rename_all`) to match the frontend type verbatim.
#[derive(Serialize, Clone)]
pub struct RadioMetadataPayload {
    pub track_id: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artwork_url: Option<String>,
}

/// Emit `player:radio-metadata`. Errors are swallowed — a dropped
/// metadata frame is cosmetic (the PlayerBar keeps the prior title)
/// and never worth interrupting playback for.
pub fn emit_radio_metadata(app: &AppHandle, payload: RadioMetadataPayload) {
    let _ = app.emit(EVENT_RADIO_METADATA, payload);
}
