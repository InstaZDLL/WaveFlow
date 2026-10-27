//! The server's library as the projection holds it: bringing it up to
//! date, reading it, and the gestures queued against it.

use serde::Serialize;

use crate::{error::AppResult, state::AppState};

use super::*;

/// What a synchronization pass did.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteSyncReport {
    pub applied: usize,
    /// Events this build did not understand and skipped. A non-zero
    /// count is not a fault — it means the server is newer than us.
    pub ignored: usize,
    pub pages: usize,
    pub cursor: i64,
    /// Whether the pass had to fall back to a fresh snapshot, either
    /// because it was the first one or because an event could not be
    /// applied.
    pub resnapshotted: bool,
}

/// Bring the projection up to date, bootstrapping if it never has been.
///
/// Answers an empty report when the profile is unbound, signed out, or
/// bound to a server without a journal — all ordinary states.
#[tauri::command]
pub async fn remote_sync_now(state: tauri::State<'_, AppState>) -> AppResult<RemoteSyncReport> {
    let report = crate::remote::sync::sync_now(&state).await?;
    Ok(RemoteSyncReport {
        applied: report.applied,
        ignored: report.ignored,
        pages: report.pages,
        cursor: report.cursor,
        resnapshotted: report.resnapshotted,
    })
}

/// Counts for the diagnostics panel. Reads the local projection only —
/// answers instantly, and answers the same whether or not the server is
/// reachable.
#[tauri::command]
pub async fn remote_get_overview(
    state: tauri::State<'_, AppState>,
) -> AppResult<crate::remote::read::RemoteOverview> {
    let Some(pool) = optional_pool(&state).await? else {
        return Ok(Default::default());
    };
    let mut conn = pool.acquire().await?;
    crate::remote::read::overview(&mut conn).await
}

/// The projected playlists, most recently touched first.
#[tauri::command]
pub async fn remote_list_playlists(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<crate::remote::read::RemotePlaylistSummary>> {
    let Some(pool) = optional_pool(&state).await? else {
        return Ok(Vec::new());
    };
    let mut conn = pool.acquire().await?;
    crate::remote::read::playlists(&mut conn).await
}

/// One playlist's tracks, in order. Entries whose metadata has not been
/// fetched yet come back with a null title rather than being skipped —
/// dropping them would make the playlist look shorter than it is.
#[tauri::command]
pub async fn remote_list_playlist_tracks(
    state: tauri::State<'_, AppState>,
    playlist_id: String,
) -> AppResult<Vec<crate::remote::read::RemoteTrack>> {
    let Some(pool) = optional_pool(&state).await? else {
        return Ok(Vec::new());
    };
    let mut conn = pool.acquire().await?;
    crate::remote::read::playlist_tracks(&mut conn, &playlist_id).await
}

/// The account's saved queue, in order.
#[tauri::command]
pub async fn remote_list_queue(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<crate::remote::read::RemoteTrack>> {
    let Some(pool) = optional_pool(&state).await? else {
        return Ok(Vec::new());
    };
    let mut conn = pool.acquire().await?;
    crate::remote::read::queue_tracks(&mut conn).await
}

/// Star or unstar a remote entity.
///
/// Answers as soon as the change is durable locally; the server hears
/// about it on the drain pass this kicks off. Offline, the entry simply
/// waits — which is the point of having a queue.
#[tauri::command]
pub async fn remote_set_favorite(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    entity_type: String,
    entity_id: String,
    starred: bool,
) -> AppResult<()> {
    crate::remote::write::set_favorite(&state, &entity_type, &entity_id, starred).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Rate a remote entity from 1 to 5, or clear it with `0`.
#[tauri::command]
pub async fn remote_set_rating(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    entity_type: String,
    entity_id: String,
    rating: u8,
) -> AppResult<()> {
    crate::remote::write::set_rating(&state, &entity_type, &entity_id, rating).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// A playable URL for a server track's Canvas, or `None` when it has none.
///
/// The now-playing view paints it behind the cover, exactly where a local
/// Canvas goes. The URL carries a **ticket** rather than a header, because a
/// `<video src>` cannot send one — the same reason remote audio is played
/// through a ticketed URL.
///
/// Do not hold on to the result: the ticket expires within the hour, so it
/// describes a permission valid now and not a property of the track.
#[tauri::command]
pub async fn remote_track_canvas(
    state: tauri::State<'_, AppState>,
    track_id: String,
) -> AppResult<Option<String>> {
    crate::remote::canvas::ticket_url(&state, &track_id).await
}

/// What the mirror holds for a server track, to fill the tag editor.
///
/// `None` when the mirror has no row for it — a track known only as an
/// identifier has no form to show.
#[tauri::command]
pub async fn remote_get_track_tags(
    state: tauri::State<'_, AppState>,
    track_id: String,
) -> AppResult<Option<crate::remote::read::RemoteTrackTags>> {
    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    crate::remote::read::track_tags(&mut conn, &track_id).await
}

/// Correct a server track's metadata.
///
/// The server keeps the correction beside the track, so it survives
/// that library's rescans and every member of the library sees it.
/// **No file is rewritten** — not here, not there — which is what
/// separates this from [`update_track_tags`] and what makes correcting
/// a track that lives on somebody else's disk possible at all.
///
/// The whole form is sent rather than the fields that changed: the
/// endpoint is wholesale, so a field left out is a correction removed.
/// [`UpdateTrackMetadata`] spells out what that rules out.
///
/// [`update_track_tags`]: crate::commands::edit::update_track_tags
/// [`UpdateTrackMetadata`]: crate::remote::mutation::Mutation::UpdateTrackMetadata
#[tauri::command]
pub async fn remote_update_track_tags(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_id: String,
    edit: crate::remote::write::TrackMetadataEdit,
) -> AppResult<()> {
    crate::remote::write::set_track_metadata(&state, &track_id, &edit).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Forget the queued changes the server refused, and report how many went.
///
/// They are never retried, so the warning that counts them was permanent: the
/// only way to clear it was to forget the server, which also throws away the
/// mirror and every change still waiting to go out. This drops the refused
/// rows and nothing else.
#[tauri::command]
pub async fn remote_discard_failed_changes(state: tauri::State<'_, AppState>) -> AppResult<u64> {
    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    crate::remote::mutation::discard_failed(&mut conn).await
}

/// Create a playlist on the remote account.
///
/// Returns the identifier it is known by locally. That is a `local:`
/// placeholder until the creation reaches the server, so a caller
/// holding it should re-read rather than assume it stays valid.
#[tauri::command]
pub async fn remote_create_playlist(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    name: String,
    track_ids: Vec<String>,
) -> AppResult<String> {
    let id = crate::remote::write::create_playlist(&state, &name, &track_ids).await?;
    crate::remote::drain::spawn(app);
    Ok(id)
}

/// Rename a playlist, set or empty its comment, change its visibility.
///
/// `clearComment` is what empties the comment: omitting the field leaves
/// it untouched, because the server coalesces an absent value onto the
/// current one.
#[tauri::command]
pub async fn remote_update_playlist(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_id: String,
    name: Option<String>,
    comment: Option<String>,
    public: Option<bool>,
    clear_comment: Option<bool>,
) -> AppResult<()> {
    crate::remote::write::update_playlist(
        &state,
        &playlist_id,
        name,
        comment,
        public,
        clear_comment.unwrap_or(false),
    )
    .await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Remove the track at `index` (its position in the current order) from a
/// remote playlist. Applies locally at once and queues the change for the
/// server.
#[tauri::command]
pub async fn remote_remove_playlist_track(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_id: String,
    index: usize,
) -> AppResult<()> {
    crate::remote::write::remove_playlist_tracks(&state, &playlist_id, &[index]).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Search the remote server's catalogue for tracks matching `query`. A
/// live query (the catalogue is the server's), capped at a page; each hit's
/// metadata is cached so adding it renders a title at once.
#[tauri::command]
pub async fn remote_search_catalogue(
    state: tauri::State<'_, AppState>,
    query: String,
) -> AppResult<Vec<crate::remote::read::RemoteTrack>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    crate::remote::catalogue::search(&state, query.trim(), 50).await
}

/// Fetch a remote album with its tracks (`GET /api/v2/albums/{id}`),
/// caching the songs so they render and play at once.
#[tauri::command]
pub async fn remote_get_album(
    state: tauri::State<'_, AppState>,
    album_id: String,
) -> AppResult<crate::remote::catalogue::RemoteAlbum> {
    crate::remote::catalogue::get_album(&state, &album_id).await
}

/// Fetch a remote artist with their albums (`GET /api/v2/artists/{id}`).
#[tauri::command]
pub async fn remote_get_artist(
    state: tauri::State<'_, AppState>,
    artist_id: String,
) -> AppResult<crate::remote::catalogue::RemoteArtist> {
    crate::remote::catalogue::get_artist(&state, &artist_id).await
}

/// Play an explicit list of remote track ids as a native queue, from
/// `start_index`. Backs "play this album": metadata comes from the cache.
#[tauri::command]
pub async fn remote_play_tracks(
    app: tauri::AppHandle,
    track_ids: Vec<String>,
    start_index: usize,
) -> AppResult<()> {
    crate::remote::playback::play_track_ids(&app, &track_ids, start_index).await
}

/// Append tracks to a remote playlist. Applies locally at once and queues
/// the additions for the server.
#[tauri::command]
pub async fn remote_add_playlist_tracks(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_id: String,
    track_ids: Vec<String>,
) -> AppResult<()> {
    crate::remote::write::add_playlist_tracks(&state, &playlist_id, &track_ids).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Move the track at `from` to `to` within a remote playlist (positions in
/// the current order). Applies locally at once and queues the new order for
/// the server.
#[tauri::command]
pub async fn remote_reorder_playlist_track(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_id: String,
    from: usize,
    to: usize,
) -> AppResult<()> {
    crate::remote::write::reorder_playlist(&state, &playlist_id, from, to).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Delete a remote playlist.
#[tauri::command]
pub async fn remote_delete_playlist(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_id: String,
) -> AppResult<()> {
    crate::remote::write::delete_playlist(&state, &playlist_id).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}
