//! Spotify Web API helpers for OAuth, token refresh, catalogue reads,
//! and playback control. Actual audio playback is owned by Spotify's
//! Web Playback SDK in the frontend; this module never receives raw
//! audio bytes.

use base64::Engine;
use chrono::Utc;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

pub const CLIENT_ID_KEY: &str = "app.spotify_client_id";
pub const CALLBACK_ADDR: &str = "127.0.0.1:49387";
pub const CALLBACK_URL: &str = "http://127.0.0.1:49387/spotify/callback";
pub const SCOPES: &str = "streaming user-read-email user-read-private user-read-playback-state user-modify-playback-state playlist-read-private playlist-read-collaborative";

const ACCOUNTS_BASE: &str = "https://accounts.spotify.com";
const API_BASE: &str = "https://api.spotify.com/v1";
const REFRESH_SKEW_MS: i64 = 60_000;

#[derive(Debug, Clone, Serialize)]
pub struct SpotifyStatus {
    pub configured: bool,
    pub connected: bool,
    pub username: Option<String>,
    pub product: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpotifyAccessToken {
    pub access_token: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpotifyArtistLite {
    pub id: String,
    pub name: String,
    pub uri: String,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpotifyAlbumLite {
    pub id: String,
    pub name: String,
    pub uri: String,
    pub image_url: Option<String>,
    pub artist_name: Option<String>,
    pub release_date: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpotifyTrackLite {
    pub id: String,
    pub name: String,
    pub uri: String,
    pub duration_ms: i64,
    pub explicit: bool,
    pub artist_name: Option<String>,
    pub album_name: Option<String>,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpotifyPlaylistLite {
    pub id: String,
    pub name: String,
    pub uri: String,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub owner_name: Option<String>,
    pub track_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpotifySearchResults {
    pub tracks: Vec<SpotifyTrackLite>,
    pub albums: Vec<SpotifyAlbumLite>,
    pub artists: Vec<SpotifyArtistLite>,
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    token_type: String,
    pub expires_in: i64,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpotifyErrorBody {
    error: SpotifyErrorDetail,
}

#[derive(Debug, Deserialize)]
struct SpotifyErrorDetail {
    message: String,
}

#[derive(Debug, Deserialize)]
struct MeResponse {
    id: String,
    display_name: Option<String>,
    product: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImageResponse {
    url: String,
    width: Option<i64>,
    height: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct OwnerResponse {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TracksTotalResponse {
    total: i64,
}

#[derive(Debug, Deserialize)]
struct ArtistResponse {
    id: String,
    name: String,
    uri: String,
    images: Option<Vec<ImageResponse>>,
}

#[derive(Debug, Deserialize)]
struct AlbumResponse {
    id: String,
    name: String,
    uri: String,
    images: Option<Vec<ImageResponse>>,
    artists: Vec<NamedResponse>,
    release_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TrackResponse {
    id: Option<String>,
    name: String,
    uri: String,
    duration_ms: i64,
    explicit: bool,
    artists: Vec<NamedResponse>,
    album: Option<AlbumResponse>,
}

#[derive(Debug, Deserialize)]
struct NamedResponse {
    name: String,
}

#[derive(Debug, Deserialize)]
struct PlaylistResponse {
    id: String,
    name: String,
    uri: String,
    description: Option<String>,
    images: Option<Vec<ImageResponse>>,
    owner: Option<OwnerResponse>,
    // `tracks` should always be present in the `me/playlists` payload,
    // but Spotify has been known to omit it for orphaned playlists
    // (3rd-party app removed, broken collaborative entry…). Treat it
    // as optional + default total = 0 so a single weird row doesn't
    // sink the whole listing.
    tracks: Option<TracksTotalResponse>,
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    items: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct PlaylistTrackItem {
    track: Option<TrackResponse>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    tracks: Option<Page<TrackResponse>>,
    albums: Option<Page<AlbumResponse>>,
    artists: Option<Page<ArtistResponse>>,
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

pub fn random_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

pub async fn read_client_id(state: &AppState) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(CLIENT_ID_KEY)
        .fetch_optional(&state.app_db)
        .await?;
    Ok(value.filter(|v| !v.trim().is_empty()))
}

pub async fn write_client_id(state: &AppState, client_id: &str) -> AppResult<()> {
    let trimmed = client_id.trim();
    if trimmed.is_empty() {
        sqlx::query("DELETE FROM app_setting WHERE key = ?")
            .bind(CLIENT_ID_KEY)
            .execute(&state.app_db)
            .await?;
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(CLIENT_ID_KEY)
    .bind(trimmed)
    .bind(now_ms())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

pub async fn status(state: &AppState) -> AppResult<SpotifyStatus> {
    let configured = read_client_id(state).await?.is_some();
    let pool = match state.require_profile_pool().await {
        Ok(p) => p,
        Err(_) => {
            return Ok(SpotifyStatus {
                configured,
                connected: false,
                username: None,
                product: None,
            });
        }
    };
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT username FROM auth_credential WHERE provider = 'spotify'")
            .fetch_optional(&pool)
            .await?;
    let username = row.and_then(|r| r.0);
    Ok(SpotifyStatus {
        configured,
        connected: username.is_some(),
        username,
        product: None,
    })
}

pub async fn exchange_code(
    client: &reqwest::Client,
    client_id: &str,
    code: &str,
    verifier: &str,
) -> AppResult<TokenResponse> {
    let res = client
        .post(format!("{ACCOUNTS_BASE}/api/token"))
        .form(&[
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", CALLBACK_URL),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|e| AppError::Other(format!("Spotify token exchange failed: {e}")))?;
    parse_spotify_response(res).await
}

pub async fn refresh_token(
    client: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> AppResult<TokenResponse> {
    let res = client
        .post(format!("{ACCOUNTS_BASE}/api/token"))
        .form(&[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|e| AppError::Other(format!("Spotify refresh failed: {e}")))?;
    parse_spotify_response(res).await
}

pub async fn store_tokens(
    state: &AppState,
    tokens: &TokenResponse,
    username: &str,
    refresh_override: Option<String>,
) -> AppResult<()> {
    if !tokens.token_type.eq_ignore_ascii_case("bearer") {
        return Err(AppError::Other("Spotify returned a non-bearer token".into()));
    }
    let pool = state.require_profile_pool().await?;
    let now = now_ms();
    let expires_at = now + tokens.expires_in.saturating_mul(1000);
    let refresh = tokens
        .refresh_token
        .clone()
        .or(refresh_override)
        .map(|s| s.into_bytes());

    sqlx::query(
        "INSERT INTO auth_credential
            (provider, username, token_encrypted, refresh_token_encrypted, expires_at, created_at, updated_at)
         VALUES ('spotify', ?, ?, ?, ?, ?, ?)
         ON CONFLICT(provider) DO UPDATE SET
            username = excluded.username,
            token_encrypted = excluded.token_encrypted,
            refresh_token_encrypted = COALESCE(excluded.refresh_token_encrypted, auth_credential.refresh_token_encrypted),
            expires_at = excluded.expires_at,
            updated_at = excluded.updated_at",
    )
    .bind(username)
    .bind(tokens.access_token.as_bytes())
    .bind(refresh)
    .bind(expires_at)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await?;
    Ok(())
}

pub async fn access_token(state: &AppState) -> AppResult<SpotifyAccessToken> {
    let client_id = read_client_id(state)
        .await?
        .ok_or_else(|| AppError::Other("Spotify Client ID is not configured".into()))?;
    let pool = state.require_profile_pool().await?;
    let row: Option<(Vec<u8>, Option<Vec<u8>>, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT token_encrypted, refresh_token_encrypted, expires_at, username
           FROM auth_credential
          WHERE provider = 'spotify'",
    )
    .fetch_optional(&pool)
    .await?;
    let Some((token_bytes, refresh_bytes, expires_at, username)) = row else {
        return Err(AppError::Other("Spotify is not connected".into()));
    };
    let access = String::from_utf8(token_bytes)
        .map_err(|_| AppError::Other("Stored Spotify access token is invalid".into()))?;
    let expires_at = expires_at.unwrap_or(0);
    if expires_at > now_ms() + REFRESH_SKEW_MS {
        return Ok(SpotifyAccessToken {
            access_token: access,
            expires_at,
        });
    }
    let refresh = refresh_bytes
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Other("Spotify refresh token is missing".into()))?;
    let client = reqwest::Client::new();
    let refreshed = refresh_token(&client, &client_id, &refresh).await?;
    store_tokens(
        state,
        &refreshed,
        username.as_deref().unwrap_or("Spotify"),
        Some(refresh),
    )
    .await?;
    Ok(SpotifyAccessToken {
        access_token: refreshed.access_token,
        expires_at: now_ms() + refreshed.expires_in.saturating_mul(1000),
    })
}

pub async fn me(client: &reqwest::Client, access_token: &str) -> AppResult<(String, Option<String>)> {
    let me: MeResponse = get_json(client, access_token, &format!("{API_BASE}/me")).await?;
    Ok((me.display_name.unwrap_or(me.id), me.product))
}

pub async fn list_playlists(
    client: &reqwest::Client,
    access_token: &str,
) -> AppResult<Vec<SpotifyPlaylistLite>> {
    // Spotify can return `null` entries inside `items` for playlists
    // that were created by a now-removed 3rd-party app or otherwise
    // orphaned in the user's library. Wrapping the page items in
    // Option<…> + flatten() filters those out cleanly instead of
    // failing the whole decode with "error decoding response body".
    let page: Page<Option<PlaylistResponse>> = get_json(
        client,
        access_token,
        &format!("{API_BASE}/me/playlists?limit=50"),
    )
    .await?;
    Ok(page
        .items
        .into_iter()
        .flatten()
        .map(SpotifyPlaylistLite::from)
        .collect())
}

pub async fn playlist_tracks(
    client: &reqwest::Client,
    access_token: &str,
    playlist_id: &str,
) -> AppResult<Vec<SpotifyTrackLite>> {
    // Dropped the `fields=` filter — since Spotify's Nov 2024 Web API
    // tightening, requesting nested fields (album images, artists,
    // ...) on certain playlists triggers a blanket 403 even when the
    // base /tracks endpoint would succeed without the filter. Asking
    // for the full payload is slightly heavier but works on every
    // playlist class the user can actually reach.
    let url = format!(
        "{API_BASE}/playlists/{}/tracks?limit=50&market=from_token",
        url_escape(playlist_id)
    );
    let page: Page<PlaylistTrackItem> = get_json(client, access_token, &url).await?;
    Ok(page
        .items
        .into_iter()
        .filter_map(|i| i.track)
        .filter_map(SpotifyTrackLite::from_track)
        .collect())
}

/// Snapshot of the user's Spotify playback queue. Contains the
/// currently playing track + the upcoming entries — exactly what
/// the WaveFlow `QueuePanel` needs when the active provider is
/// Spotify and the local SQLite queue is empty.
#[derive(Debug, Clone, Serialize)]
pub struct SpotifyQueueSnapshot {
    pub current: Option<SpotifyTrackLite>,
    pub upcoming: Vec<SpotifyTrackLite>,
}

#[derive(Debug, Deserialize)]
struct QueueResponse {
    currently_playing: Option<TrackResponse>,
    queue: Vec<TrackResponse>,
}

/// Fetch the user's playback queue via `GET /me/player/queue`.
/// Spotify returns `currently_playing` + an array of upcoming tracks
/// (capped at ~20 items). Requires an active Connect device — when
/// nothing is playing the response is `{"currently_playing": null,
/// "queue": []}` which we surface as an empty snapshot.
pub async fn queue(
    client: &reqwest::Client,
    access_token: &str,
) -> AppResult<SpotifyQueueSnapshot> {
    let res: QueueResponse =
        get_json(client, access_token, &format!("{API_BASE}/me/player/queue")).await?;
    Ok(SpotifyQueueSnapshot {
        current: res.currently_playing.and_then(SpotifyTrackLite::from_track),
        upcoming: res
            .queue
            .into_iter()
            .filter_map(SpotifyTrackLite::from_track)
            .collect(),
    })
}

pub async fn search(
    client: &reqwest::Client,
    access_token: &str,
    query: &str,
) -> AppResult<SpotifySearchResults> {
    let encoded = url_escape(query);
    let res: SearchResponse = get_json(
        client,
        access_token,
        &format!("{API_BASE}/search?q={encoded}&type=track,album,artist&limit=12"),
    )
    .await?;
    Ok(SpotifySearchResults {
        tracks: res
            .tracks
            .map(|p| {
                p.items
                    .into_iter()
                    .filter_map(SpotifyTrackLite::from_track)
                    .collect()
            })
            .unwrap_or_default(),
        albums: res
            .albums
            .map(|p| p.items.into_iter().map(SpotifyAlbumLite::from).collect())
            .unwrap_or_default(),
        artists: res
            .artists
            .map(|p| p.items.into_iter().map(SpotifyArtistLite::from).collect())
            .unwrap_or_default(),
    })
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    access_token: &str,
    url: &str,
) -> AppResult<T> {
    let res = client
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| AppError::Other(format!("Spotify API request failed: {e}")))?;
    parse_spotify_response(res).await
}

async fn parse_spotify_response<T: for<'de> Deserialize<'de>>(
    res: reqwest::Response,
) -> AppResult<T> {
    let status = res.status();
    if status == StatusCode::NO_CONTENT {
        return Err(AppError::Other("Spotify returned an empty response".into()));
    }
    if status.is_success() {
        return res
            .json::<T>()
            .await
            .map_err(|e| AppError::Other(format!("Spotify response parse failed: {e}")));
    }
    let text = res.text().await.unwrap_or_default();
    let message = serde_json::from_str::<SpotifyErrorBody>(&text)
        .map(|b| b.error.message)
        .unwrap_or_else(|_| text.clone());
    // Spotify started returning a blanket 403 for several endpoints
    // when the requesting app isn't on Extended Quota Mode (since
    // Nov 2024). The bare "Forbidden" message is unhelpful — surface
    // the most common cause so the user knows it isn't necessarily
    // a bug on our side.
    let friendly = if status == StatusCode::FORBIDDEN {
        format!(
            "Spotify a refusé la requête (403 Forbidden). \
             Cause probable : depuis novembre 2024, Spotify bloque l'accès \
             aux playlists éditoriales (Daily Mix, Discover Weekly, playlists \
             de marque…) et à certains endpoints (Audio Features, recommandations) \
             pour les apps tierces. Détail Spotify : {message}"
        )
    } else {
        format!("Spotify API error {}: {}", status.as_u16(), message)
    };
    tracing::warn!(status = %status, body = %text, "spotify API error");
    Err(AppError::Other(friendly))
}

fn best_image(images: Option<Vec<ImageResponse>>) -> Option<String> {
    images
        .unwrap_or_default()
        .into_iter()
        .max_by_key(|i| i.width.unwrap_or(0) * i.height.unwrap_or(0))
        .map(|i| i.url)
}

fn first_artist(artists: &[NamedResponse]) -> Option<String> {
    artists.first().map(|a| a.name.clone())
}

fn url_escape(input: &str) -> String {
    url::form_urlencoded::byte_serialize(input.as_bytes()).collect()
}

impl From<ArtistResponse> for SpotifyArtistLite {
    fn from(value: ArtistResponse) -> Self {
        Self {
            id: value.id,
            name: value.name,
            uri: value.uri,
            image_url: best_image(value.images),
        }
    }
}

impl From<AlbumResponse> for SpotifyAlbumLite {
    fn from(value: AlbumResponse) -> Self {
        let artist_name = first_artist(&value.artists);
        Self {
            id: value.id,
            name: value.name,
            uri: value.uri,
            image_url: best_image(value.images),
            artist_name,
            release_date: value.release_date,
        }
    }
}

impl SpotifyTrackLite {
    fn from_track(value: TrackResponse) -> Option<Self> {
        let id = value.id?;
        let album_name = value.album.as_ref().map(|a| a.name.clone());
        let image_url = value.album.and_then(|a| best_image(a.images));
        Some(Self {
            id,
            name: value.name,
            uri: value.uri,
            duration_ms: value.duration_ms,
            explicit: value.explicit,
            artist_name: first_artist(&value.artists),
            album_name,
            image_url,
        })
    }
}

impl From<PlaylistResponse> for SpotifyPlaylistLite {
    fn from(value: PlaylistResponse) -> Self {
        Self {
            id: value.id,
            name: value.name,
            uri: value.uri,
            description: value.description,
            image_url: best_image(value.images),
            owner_name: value.owner.and_then(|o| o.display_name),
            track_count: value.tracks.map(|t| t.total).unwrap_or(0),
        }
    }
}
