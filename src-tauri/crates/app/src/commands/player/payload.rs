//! What the frontend is sent about the player and the tracks of its queue.

use serde::Serialize;

use crate::{
    audio::state::SharedPlayback,
    paths::AppPaths,
    queue::{self, QueueTrack},
    state::AppState,
};

/// Snapshot of the player state, returned to the frontend on demand.
///
/// `current_track` is populated from the persisted resume point at
/// startup (before the user has played anything), so the PlayerBar
/// can render the last-played metadata while the engine is still
/// Idle. Once playback starts it still reflects the currently
/// loaded track via `queue::restore_state`.
#[derive(Debug, Serialize)]
pub struct PlayerStateSnapshot {
    pub state: String,
    pub position_ms: u64,
    pub volume: f32,
    pub sample_rate: u32,
    pub channels: u16,
    pub shuffle: bool,
    /// `"off"` / `"tracks"` / `"albums"` (#618). `shuffle` stays the
    /// plain on/off, because that is all an MPD client can express.
    pub shuffle_mode: String,
    pub repeat_mode: String,
    pub current_track: Option<QueueTrackPayload>,
    /// True when the active output is shipping native DSD via DoP
    /// (#495) — reflects what really engaged, not just the opt-in.
    pub dop_active: bool,
    /// True when the stream really owns the device — WASAPI Exclusive
    /// on Windows, a raw `hw:` ALSA device on Linux, CoreAudio hog mode
    /// on macOS. Read from the engine's *active* state, not the opt-in,
    /// so it is false after a silent fallback to shared mode and on any
    /// platform with no exclusive backend. Without it the UI cannot tell
    /// a stream that reaches the DAC untouched from one the system mixer
    /// re-clocks on its way there, which is the difference between
    /// bit-perfect and merely un-processed.
    pub exclusive_active: bool,
    /// What the output really is right now, as one value (#597): shared,
    /// exclusive, native DoP, or exclusive asked for and refused. The
    /// badge in the player reads this rather than re-deriving it from the
    /// two flags above, which is how the UI and the engine used to end up
    /// disagreeing about what "engaged" meant.
    pub output_mode: crate::audio::OutputMode,
}

/// Subset of [`crate::queue::QueueTrack`] flattened into the shape
/// the frontend expects for `Track` display (title / artist / album
/// / artwork path). We rebuild the artwork absolute path on the way
/// out so the UI can pass it straight to the asset protocol.
#[derive(Debug, Clone, Serialize)]
pub struct QueueTrackPayload {
    pub id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub album_title: Option<String>,
    /// See [`QueueTrack::album_id`]: the manual motion cover and the
    /// album link are keyed by it (#766).
    pub album_id: Option<i64>,
    pub duration_ms: i64,
    pub file_path: String,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    /// Audio quality fields used by the PlayerBar footer / Hi-Res
    /// badge. Carried through here so the frontend doesn't have to
    /// issue a separate `list_tracks` lookup just to learn the
    /// codec of what's playing.
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub bit_depth: Option<i64>,
    pub codec: Option<String>,
    pub file_size: i64,
}

impl PlayerStateSnapshot {
    pub(super) fn from_shared(
        shared: &SharedPlayback,
        shuffle: queue::ShuffleMode,
        repeat_mode: queue::RepeatMode,
        current_track: Option<QueueTrackPayload>,
        dop_active: bool,
        exclusive_active: bool,
        output_mode: crate::audio::OutputMode,
    ) -> Self {
        Self {
            state: shared.state().as_str().to_string(),
            position_ms: shared.current_position_ms(),
            volume: shared.volume(),
            sample_rate: shared
                .sample_rate
                .load(std::sync::atomic::Ordering::Relaxed),
            channels: shared.channels.load(std::sync::atomic::Ordering::Relaxed),
            shuffle: shuffle.is_on(),
            shuffle_mode: shuffle.as_str().to_string(),
            repeat_mode: repeat_mode.as_str().to_string(),
            current_track,
            dop_active,
            exclusive_active,
            output_mode,
        }
    }
}

/// Build a [`QueueTrackPayload`] from a [`crate::queue::QueueTrack`],
/// resolving the artwork absolute path against the active profile's
/// data directory. Returns `None` if the profile paths aren't ready.
pub(super) fn queue_track_to_payload(
    state: &AppState,
    track: QueueTrack,
    profile_id: Option<i64>,
) -> QueueTrackPayload {
    queue_track_to_payload_with_paths(&state.paths, track, profile_id)
}

pub(super) fn queue_track_to_payload_with_paths(
    paths: &AppPaths,
    track: QueueTrack,
    profile_id: Option<i64>,
) -> QueueTrackPayload {
    let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
        track.artwork_hash.as_deref(),
        track.artwork_format.as_deref(),
        profile_id,
    ) {
        (Some(hash), Some(format), Some(pid)) => {
            let dir = paths.profile_artwork_dir(pid);
            let full = dir
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string();
            let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&dir, hash);
            (Some(full), p1, p2)
        }
        _ => (None, None, None),
    };
    QueueTrackPayload {
        id: track.id,
        title: track.title,
        artist_id: track.artist_id,
        artist_name: track.artist_name,
        artist_ids: track.artist_ids,
        album_title: track.album_title,
        album_id: track.album_id,
        duration_ms: track.duration_ms,
        file_path: track.file_path,
        bitrate: track.bitrate,
        sample_rate: track.sample_rate,
        channels: track.channels,
        bit_depth: track.bit_depth,
        codec: track.codec,
        file_size: track.file_size,
        artwork_path,
        artwork_path_1x,
        artwork_path_2x,
    }
}
