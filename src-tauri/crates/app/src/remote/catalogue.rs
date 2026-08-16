//! Searching the remote server's catalogue (RFC-005).
//!
//! Unlike the projection reads, this is a live query — the catalogue is
//! the server's, and only the playlists / favourites / etc. the account
//! touched are ever projected locally. Backs "add tracks to a remote
//! playlist": the results are cached into `remote_track` as they come in,
//! so the moment one is added the playlist can render its title instead of
//! waiting for a backfill.

use serde::Deserialize;

use crate::{
    error::{AppError, AppResult},
    remote::{client::RemoteClient, dto::SongItem, read::RemoteTrack},
    state::AppState,
};

/// Only the `songs` bucket of the server's search response matters here —
/// adding to a playlist is a track gesture. Artists / albums are ignored.
#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    songs: Vec<SongItem>,
}

/// Search the catalogue for tracks matching `query`, newest FTS rank first.
/// Each hit's metadata is cached so a subsequent add shows its title at
/// once. Returns the display rows for the picker.
pub async fn search(state: &AppState, query: &str, limit: i64) -> AppResult<Vec<RemoteTrack>> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    let client = RemoteClient::try_build(state)
        .await?
        .ok_or_else(|| AppError::Other("not signed in to a remote server".into()))?;

    let resp: SearchResponse = client
        .send_json(
            client
                .request(reqwest::Method::GET, "/api/v2/search")
                .query(&[("q", query), ("limit", &limit.to_string())]),
        )
        .await
        .map_err(|err| AppError::Other(format!("search: {}", err.message)))?;

    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    let mut tracks = Vec::with_capacity(resp.songs.len());
    for song in &resp.songs {
        crate::remote::projection::cache_song(&mut conn, song).await?;
        tracks.push(RemoteTrack {
            id: song.id.clone(),
            title: song.title.clone(),
            artist: song.artist.clone(),
            album: song.album.clone(),
            duration_ms: song.duration_ms,
            artwork_hash: song.artwork_hash.clone(),
        });
    }
    Ok(tracks)
}
