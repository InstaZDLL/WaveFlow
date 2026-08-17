//! Searching the remote server's catalogue (RFC-005).
//!
//! Unlike the projection reads, this is a live query — the catalogue is
//! the server's, and only the playlists / favourites / etc. the account
//! touched are ever projected locally. Backs "add tracks to a remote
//! playlist": the results are cached into `remote_track` as they come in,
//! so the moment one is added the playlist can render its title instead of
//! waiting for a backfill.

use serde::{Deserialize, Serialize};

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

/// The server's `AlbumDetail` flattens `AlbumItem` at the top level and
/// carries the album's songs. We keep the fields the album view shows.
#[derive(Deserialize)]
struct AlbumDetailResponse {
    id: String,
    title: String,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    artist_id: Option<String>,
    #[serde(default)]
    artwork_hash: Option<String>,
    #[serde(default)]
    year: Option<i64>,
    #[serde(default)]
    songs: Vec<SongItem>,
}

/// A remote album with its tracks, for the album detail view.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteAlbum {
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub artwork_hash: Option<String>,
    pub year: Option<i64>,
    pub tracks: Vec<RemoteTrack>,
}

/// The server's `ArtistDetail` flattens `ArtistItem` and lists the artist's
/// albums (no flat track list).
#[derive(Deserialize)]
struct ArtistDetailResponse {
    id: String,
    name: String,
    #[serde(default)]
    artwork_hash: Option<String>,
    #[serde(default)]
    albums: Vec<AlbumSummaryDto>,
}

#[derive(Deserialize)]
struct AlbumSummaryDto {
    id: String,
    title: String,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    artwork_hash: Option<String>,
    #[serde(default)]
    year: Option<i64>,
}

/// A remote artist with their albums, for the artist detail view.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteArtist {
    pub id: String,
    pub name: String,
    pub artwork_hash: Option<String>,
    pub albums: Vec<RemoteAlbumSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteAlbumSummary {
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub artwork_hash: Option<String>,
    pub year: Option<i64>,
}

fn song_to_track(song: &SongItem) -> RemoteTrack {
    RemoteTrack {
        id: song.id.clone(),
        title: song.title.clone(),
        artist: song.artist.clone(),
        artist_id: song.artist_id.clone(),
        album: song.album.clone(),
        album_id: song.album_id.clone(),
        duration_ms: song.duration_ms,
        artwork_hash: song.artwork_hash.clone(),
        // The projection is the source of truth for the star; a fresh
        // server row doesn't know about a pending local change. Callers
        // that need it accurate (the album view) fill it from
        // `remote_favorite`; search results don't show a heart.
        starred: false,
    }
}

/// Search the catalogue for tracks matching `query`, newest FTS rank first.
/// Each hit's metadata is cached so a subsequent add shows its title at
/// once. Returns the display rows for the picker.
pub async fn search(state: &AppState, query: &str, limit: i64) -> AppResult<Vec<RemoteTrack>> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    // Pin the client and the cache pool to one profile for the whole call,
    // so a switch mid-request can't read one profile's session and cache
    // into another's tables.
    let profile_id = state.require_profile_id().await?;
    let client = RemoteClient::try_build_for(state, profile_id)
        .await?
        .ok_or_else(|| AppError::Other("not signed in to a remote server".into()))?;

    let resp: SearchResponse = client
        .send_json(
            client
                .request(reqwest::Method::GET, "/api/v2/search")
                .query(&[("q", query), ("limit", &limit.to_string())]),
        )
        .await
        // Surface the whole failure (status + code + message), not just the
        // message: a 401 here means the session lapsed, which the caller
        // must be able to tell apart from a plain "search failed" to prompt
        // a re-auth rather than shrug.
        .map_err(|err| AppError::Other(format!("search failed: {err}")))?;

    let pool = state.require_profile_pool_for(Some(profile_id)).await?;
    let mut tracks = Vec::with_capacity(resp.songs.len());
    // Cache every hit in one transaction: the rows exist only to label the
    // picker, and committing each individually multiplies the fsyncs for no
    // benefit.
    let mut tx = pool.begin().await?;
    for song in &resp.songs {
        crate::remote::projection::cache_song(&mut tx, song).await?;
        tracks.push(song_to_track(song));
    }
    tx.commit().await?;
    Ok(tracks)
}

/// Fetch a remote album with its tracks. Caches each song so the album's
/// tracks are playable and labelled at once (and so a later "play album"
/// finds them in the cache).
pub async fn get_album(state: &AppState, album_id: &str) -> AppResult<RemoteAlbum> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    // Pin the client and the cache pool to one profile (see `search`).
    let profile_id = state.require_profile_id().await?;
    let client = RemoteClient::try_build_for(state, profile_id)
        .await?
        .ok_or_else(|| AppError::Other("not signed in to a remote server".into()))?;

    let resp: AlbumDetailResponse = client
        .send_json(client.get(&format!("/api/v2/albums/{album_id}")))
        .await
        // Preserve the status (see `search`) so a lapsed session is
        // distinguishable from a missing album.
        .map_err(|err| AppError::Other(format!("album fetch failed: {err}")))?;

    let pool = state.require_profile_pool_for(Some(profile_id)).await?;
    let mut tracks = Vec::with_capacity(resp.songs.len());
    // Cache the album's songs and read back the favorites in one
    // transaction, so the tracks are cached-and-labelled atomically.
    let mut tx = pool.begin().await?;
    for song in &resp.songs {
        crate::remote::projection::cache_song(&mut tx, song).await?;
        tracks.push(song_to_track(song));
    }

    // Fill the star from the synced favorites — the album's own rows carry
    // the server's view, which can lag a pending local like/unlike.
    let starred: std::collections::HashSet<String> =
        sqlx::query_scalar("SELECT entity_id FROM remote_favorite WHERE entity_type = 'track'")
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();
    tx.commit().await?;
    for track in &mut tracks {
        track.starred = starred.contains(&track.id);
    }

    Ok(RemoteAlbum {
        id: resp.id,
        title: resp.title,
        artist: resp.artist,
        artist_id: resp.artist_id,
        artwork_hash: resp.artwork_hash,
        year: resp.year,
        tracks,
    })
}

/// Fetch a remote artist with their albums (`GET /api/v2/artists/{id}`).
/// The server carries the artist's image (`artwork_hash`) but no biography,
/// so the view fills the bio from Last.fm by name.
pub async fn get_artist(state: &AppState, artist_id: &str) -> AppResult<RemoteArtist> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    let client = RemoteClient::try_build(state)
        .await?
        .ok_or_else(|| AppError::Other("not signed in to a remote server".into()))?;

    let resp: ArtistDetailResponse = client
        .send_json(client.get(&format!("/api/v2/artists/{artist_id}")))
        .await
        // Preserve the status (see `search`) so a lapsed session is
        // distinguishable from a missing artist.
        .map_err(|err| AppError::Other(format!("artist fetch failed: {err}")))?;

    Ok(RemoteArtist {
        id: resp.id,
        name: resp.name,
        artwork_hash: resp.artwork_hash,
        albums: resp
            .albums
            .into_iter()
            .map(|a| RemoteAlbumSummary {
                id: a.id,
                title: a.title,
                artist: a.artist,
                artwork_hash: a.artwork_hash,
                year: a.year,
            })
            .collect(),
    })
}
