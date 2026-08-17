//! Tauri commands for binding a profile to a remote server (RFC-005).
//!
//! Deliberately small: identifying a server, signing in, and the two
//! ways of undoing that. Everything the projection does sits behind the
//! binding these commands establish.
//!
//! ## Two ways out, because they are not the same intent
//!
//! [`remote_sign_out`] drops the credentials only. The binding and the
//! projection survive, so the cached remote library stays readable and
//! signing back into the same account resumes from its cursor.
//!
//! [`remote_forget_server`] drops everything, pending writes included.
//! That is the destructive one, and the UI must present it as such.

use serde::Serialize;

use crate::{
    error::{AppError, AppResult},
    remote::{
        auth,
        binding::{self, RemoteIdentity},
        probe::{self, ServerFlavour},
        tokens,
    },
    state::AppState,
};

/// What the Settings card renders. One round-trip rather than a chain
/// of `getUrl` + `isSignedIn` calls.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteStatus {
    /// `None` means this profile is local-only, which is the normal
    /// state, not a fault.
    pub server_url: Option<String>,
    /// `"waveflow"` or `"subsonic"`, absent when unbound.
    pub flavour: Option<String>,
    /// The signed-in account's name, when we know it.
    pub username: Option<String>,
    /// Whether usable credentials exist. Does not prove the server still
    /// accepts them — that is one HTTP round-trip away and belongs to
    /// whoever needs the fresher signal.
    pub signed_in: bool,
    /// Whether a first snapshot has ever been applied. Distinguishes an
    /// empty account from a bootstrap that never ran.
    pub bootstrapped: bool,
}

/// What a probe found, before any credential is asked for.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteProbeResult {
    pub flavour: String,
    /// Whatever the server calls itself, kept verbatim for display.
    pub server_type: Option<String>,
    pub server_version: Option<String>,
    /// Whether the journal, per-device acknowledgement and mutation
    /// idempotency exist here.
    pub supports_sync: bool,
}

#[tauri::command]
pub async fn remote_get_status(state: tauri::State<'_, AppState>) -> AppResult<RemoteStatus> {
    status(&state).await
}

/// Identify the server behind a URL without signing in.
///
/// Useful before the user commits to anything: a native server signs in
/// through the browser, a third-party one with a username and password,
/// and telling them apart afterwards would mean showing the wrong form
/// first.
#[tauri::command]
pub async fn remote_detect_server(url: String) -> AppResult<RemoteProbeResult> {
    let flavour = probe::detect(url.trim()).await?;
    Ok(match flavour {
        ServerFlavour::Waveflow { server_version } => RemoteProbeResult {
            flavour: "waveflow".into(),
            server_type: Some("waveflow".into()),
            server_version,
            supports_sync: true,
        },
        ServerFlavour::Subsonic {
            server_type,
            server_version,
        } => RemoteProbeResult {
            flavour: "subsonic".into(),
            server_type,
            server_version,
            supports_sync: false,
        },
    })
}

/// Run the browser handshake and bind the active profile.
///
/// Blocks for as long as the user takes to consent, up to three minutes.
/// The frontend should show this as a pending state rather than an
/// unresponsive one.
#[tauri::command]
pub async fn remote_begin_login(
    state: tauri::State<'_, AppState>,
    url: String,
) -> AppResult<RemoteStatus> {
    auth::begin_login(&state, &url).await?;
    status(&state).await
}

/// Drop the credentials, keep everything else.
#[tauri::command]
pub async fn remote_sign_out(state: tauri::State<'_, AppState>) -> AppResult<RemoteStatus> {
    auth::sign_out(&state).await?;
    status(&state).await
}

/// Drop the credentials, the binding, the projection and every write
/// that never reached the server.
#[tauri::command]
pub async fn remote_forget_server(state: tauri::State<'_, AppState>) -> AppResult<RemoteStatus> {
    auth::forget_server(&state).await?;
    status(&state).await
}

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

/// Record a play against the remote account.
///
/// `submission: false` is a "now playing" ping; only a completed listen
/// enters the local history.
///
/// **The identifier must be the server's.** Nothing calls this from
/// playback, and it should not until remote playback exists: the local
/// player deals in file paths and local row ids, which the server
/// validates and rejects.
#[tauri::command]
pub async fn remote_scrobble(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_id: String,
    submission: bool,
    played_at: Option<i64>,
) -> AppResult<()> {
    crate::remote::write::scrobble(&state, &track_id, submission, played_at).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Save the account's play queue. Same identifier caveat as
/// [`remote_scrobble`].
#[tauri::command]
pub async fn remote_save_queue(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_ids: Vec<String>,
    current: Option<String>,
    position_ms: i64,
    client: Option<String>,
) -> AppResult<()> {
    crate::remote::write::save_queue(&state, &track_ids, current, position_ms, client).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Publish a share of remote tracks.
///
/// Returns the local identifier. The public link is not known until the
/// creation reaches the server — the token is derived from a server-side
/// secret — so a share made offline has no link to copy yet.
#[tauri::command]
pub async fn remote_create_share(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_ids: Vec<String>,
    description: Option<String>,
    expires_at: Option<i64>,
) -> AppResult<String> {
    let id = crate::remote::write::create_share(&state, &track_ids, description, expires_at).await?;
    crate::remote::drain::spawn(app);
    Ok(id)
}

/// Change a share's description or expiry, or empty either.
///
/// The two `clear*` flags are the only way to empty a field: omitting it
/// leaves it in place, so an expiry set by mistake would otherwise be
/// permanent.
#[tauri::command]
pub async fn remote_update_share(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    share_id: String,
    description: Option<String>,
    expires_at: Option<i64>,
    clear_description: Option<bool>,
    clear_expires_at: Option<bool>,
) -> AppResult<()> {
    crate::remote::write::update_share(
        &state,
        &share_id,
        description,
        expires_at,
        clear_description.unwrap_or(false),
        clear_expires_at.unwrap_or(false),
    )
    .await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Withdraw a share.
#[tauri::command]
pub async fn remote_delete_share(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    share_id: String,
) -> AppResult<()> {
    crate::remote::write::delete_share(&state, &share_id).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// The leased pool of the active profile, or `None` when there is no
/// active profile.
///
/// No profile is an ordinary state for these reads — onboarding queries
/// them before one exists — so it answers empty rather than failing.
///
/// Returns the *lease*, not a bare connection: the caller acquires its
/// connection from the returned pool and keeps the lease bound for the
/// whole read, so a profile switch can't close the pool under a live
/// connection (CLAUDE.md "profile-scoped pool").
async fn optional_pool(state: &AppState) -> AppResult<Option<crate::state::ProfilePool>> {
    match state.require_profile_pool().await {
        Ok(pool) => Ok(Some(pool)),
        Err(AppError::NoActiveProfile) => Ok(None),
        Err(err) => Err(err),
    }
}

async fn status(state: &AppState) -> AppResult<RemoteStatus> {
    // No active profile is a legitimate state here — the onboarding
    // screens can query this before one exists — so it answers "unbound"
    // rather than failing. Anything else is a real fault and propagates.
    let pool = match state.require_profile_pool().await {
        Ok(pool) => pool,
        Err(AppError::NoActiveProfile) => {
            return Ok(RemoteStatus {
                server_url: None,
                flavour: None,
                username: None,
                signed_in: false,
                bootstrapped: false,
            })
        }
        Err(err) => return Err(err),
    };
    let mut conn = pool.acquire().await?;

    let binding = binding::read(&mut conn).await?;
    let credentials = tokens::read(&mut conn).await?;

    Ok(RemoteStatus {
        server_url: binding.as_ref().map(|b| b.server_url.clone()),
        flavour: binding.as_ref().map(|b| b.identity.flavour().to_string()),
        username: credentials
            .as_ref()
            .and_then(|c| c.username.clone())
            .or_else(|| match binding.as_ref().map(|b| &b.identity) {
                Some(RemoteIdentity::Subsonic { username }) => Some(username.clone()),
                _ => None,
            }),
        signed_in: credentials.is_some(),
        bootstrapped: binding
            .as_ref()
            .is_some_and(|b| b.bootstrapped_at.is_some()),
    })
}

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

/// Fetch a remote track's artwork (by hash) as a `data:` URL — the artwork
/// endpoint is Bearer-only, so a bare `<img src>` to it would 401.
#[tauri::command]
pub async fn remote_artwork(
    state: tauri::State<'_, AppState>,
    artwork_hash: String,
) -> AppResult<String> {
    crate::remote::stream::artwork_data_url(&state, &artwork_hash).await
}

/// Play a projected remote playlist as a native queue, starting at
/// `start_index`. Fills the in-memory remote queue from the projection so
/// the tracks after it auto-advance, mints a stream ticket for the first,
/// and hands its URL to the engine. Manual next/previous and end-of-track
/// all route back through [`crate::remote::playback`] while the session is
/// active.
#[tauri::command]
pub async fn remote_play_playlist(
    app: tauri::AppHandle,
    playlist_id: String,
    start_index: usize,
) -> AppResult<()> {
    crate::remote::playback::start(&app, &playlist_id, start_index).await
}

/// One row of the live remote play queue.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteQueueRow {
    pub id: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    /// Server id of the primary artist — lets the "About the artist" panel
    /// link to the remote artist and fetch its photo for a remote track.
    pub artist_id: Option<String>,
    pub artwork_hash: Option<String>,
    pub duration_ms: Option<i64>,
}

/// The live remote play queue and its cursor, for the queue panel.
#[derive(Debug, Clone, Serialize)]
pub struct RemotePlayQueue {
    pub entries: Vec<RemoteQueueRow>,
    pub index: usize,
}

/// Snapshot the active remote play queue, or `None` when the current
/// playback is a library track or a radio stream. Read from memory —
/// instant, no server round-trip.
#[tauri::command]
pub async fn remote_get_play_queue(
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<RemotePlayQueue>> {
    Ok(state.remote_playback.snapshot().map(|(entries, index)| {
        RemotePlayQueue {
            entries: entries
                .into_iter()
                .map(|e| RemoteQueueRow {
                    id: e.id,
                    title: e.title,
                    artist: e.artist,
                    artist_id: e.artist_id,
                    artwork_hash: e.artwork_hash,
                    duration_ms: e.duration_ms,
                })
                .collect(),
            index,
        }
    }))
}

/// Jump the remote play queue to an absolute position and play it. Backs
/// the queue panel's click-to-jump; a no-op when no remote session is
/// active.
#[tauri::command]
pub async fn remote_queue_jump(app: tauri::AppHandle, index: usize) -> AppResult<()> {
    crate::remote::playback::jump_to(&app, index).await
}
