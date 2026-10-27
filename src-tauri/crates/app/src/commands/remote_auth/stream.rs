//! Streaming a server track: its URL, the transcode status, the artwork
//! and the caches behind both.

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Mint a locally-playable stream URL for a projected remote track. The
/// server hands back a sealed ticket; this returns
/// `{base_url}/api/v2/stream/<ticket>`, ready for `player_play_url`.
#[tauri::command]
pub async fn remote_stream_url(
    state: tauri::State<'_, AppState>,
    track_id: String,
) -> AppResult<String> {
    crate::remote::stream::ticket_url(&state, &track_id).await
}

/// What the bound server's transcoder can do, and how busy it is.
///
/// Read by the settings card so the preference can say plainly that a server
/// without FFmpeg cannot honour it — otherwise the only way to learn that is
/// to turn transcoding on and hear the original anyway, with nothing on
/// screen explaining why.
#[tauri::command]
pub async fn remote_transcode_status(
    state: tauri::State<'_, AppState>,
) -> AppResult<crate::remote::stream::TranscodeStatus> {
    crate::remote::stream::status(&state).await
}

/// Resolve a remote cover (by hash) to a **local file path**, downloading it
/// once if it is not already cached.
///
/// The artwork endpoint is Bearer-only, so a bare `<img src>` to it answers
/// 401. This used to be worked around by inlining the bytes as a `data:` URL;
/// caching to disk instead lets the webview's asset protocol serve the file
/// exactly like a scanned local cover — no base64 held in the renderer, and a
/// second launch paints from disk rather than re-downloading.
#[tauri::command]
pub async fn remote_artwork(
    state: tauri::State<'_, AppState>,
    artwork_hash: String,
) -> AppResult<String> {
    let path = crate::remote::artwork::cached_path(&state, &artwork_hash).await?;
    Ok(path.to_string_lossy().into_owned())
}

/// What the remote cover cache holds on disk, for the settings card.
#[tauri::command]
pub async fn remote_artwork_cache_info(
    state: tauri::State<'_, AppState>,
) -> AppResult<crate::remote::artwork::ArtworkCacheInfo> {
    crate::remote::artwork::info(&state).await
}

/// Delete every cached remote cover. Costs one download each time one is
/// looked at again — the images are content-addressed, so nothing is lost.
#[tauri::command]
pub async fn remote_clear_artwork_cache(state: tauri::State<'_, AppState>) -> AppResult<()> {
    crate::remote::artwork::clear(&state).await
}

/// How much disk the cached remote streams occupy, and how many tracks that
/// is. Reported beside the cover cache but counted separately: whole songs
/// and thumbnails differ in size by two orders of magnitude, and one figure
/// covering both would be read as the smaller one.
#[tauri::command]
pub async fn remote_stream_cache_info(
    state: tauri::State<'_, AppState>,
) -> AppResult<StreamCacheInfo> {
    // One snapshot, held across the walk: the cached audio is this profile's
    // user data, and a switch landing mid-call must not have us report another
    // profile's disk under this one's name.
    let (_pool, profile_id) = state.require_profile_snapshot().await?;
    let dir = state.paths.profile_remote_stream_dir(profile_id);
    let (bytes, tracks) =
        tokio::task::spawn_blocking(move || crate::audio::stream_cache::info(&dir))
            .await
            .map_err(|err| AppError::Other(format!("stream cache info: {err}")))?;
    Ok(StreamCacheInfo { bytes, tracks })
}

/// Bytes held by the remote stream cache, and how many complete tracks.
#[derive(serde::Serialize)]
pub struct StreamCacheInfo {
    pub bytes: u64,
    pub tracks: usize,
}

/// Delete every cached remote stream. Costs one download each time a track is
/// played again — nothing is lost, since the server still holds the bytes.
#[tauri::command]
pub async fn remote_clear_stream_cache(state: tauri::State<'_, AppState>) -> AppResult<usize> {
    let (_pool, profile_id) = state.require_profile_snapshot().await?;
    let dir = state.paths.profile_remote_stream_dir(profile_id);
    // Deleting gigabytes of files is a blocking walk; run it where blocking is
    // allowed, as `artwork::clear` beside it already does. On the async
    // executor it would stall every other command for the duration.
    tokio::task::spawn_blocking(move || crate::audio::stream_cache::clear(&dir))
        .await
        .map_err(|err| AppError::Other(format!("stream cache clear: {err}")))?
        .map_err(AppError::from)
}
