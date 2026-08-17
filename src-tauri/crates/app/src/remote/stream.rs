//! Playback helpers for the remote source (RFC-005).
//!
//! A projected [`RemoteTrack`](super::read::RemoteTrack) carries metadata
//! but no audio: the server's tracks have no local file. To play one, the
//! server mints a **stream ticket** — a sealed, time-limited credential
//! (TTL ~1h) — which authorises the ticketed stream endpoint on its own,
//! so `<audio src>` / [`HttpMediaSource`](crate::audio::http_source) can
//! fetch it without an `Authorization` header.
//!
//! ## The URL is built from our base, never the server's
//!
//! `POST /tracks/{id}/stream-ticket` deliberately answers with a
//! **relative** path (`/api/v2/stream/<ticket>`). We prepend the binding's
//! own `base_url` and reject anything absolute or protocol-relative: an
//! absolute URL from the server could redirect playback to a host the user
//! never authenticated against.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;

use crate::{
    error::{AppError, AppResult},
    remote::client::RemoteClient,
    state::AppState,
};

#[derive(Deserialize)]
struct StreamTicketResponse {
    /// Relative, e.g. `/api/v2/stream/<ticket>`.
    url: String,
}

async fn client(state: &AppState) -> AppResult<RemoteClient> {
    RemoteClient::try_build(state)
        .await?
        .ok_or_else(|| AppError::Other("not signed in to a remote server".into()))
}

/// Mint a stream ticket for a remote track and return a locally-playable
/// absolute URL (`{base_url}/api/v2/stream/<ticket>`), safe to hand to
/// `player_play_url` / `HttpMediaSource`.
pub async fn ticket_url(state: &AppState, track_id: &str) -> AppResult<String> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline".into()));
    }
    let client = client(state).await?;

    let resp: StreamTicketResponse = client
        .send_json(client.request(
            reqwest::Method::POST,
            &format!("/api/v2/tracks/{track_id}/stream-ticket"),
        ))
        .await
        .map_err(|err| AppError::Other(format!("stream ticket: {}", err.message)))?;

    // Trust only our own base. A relative path is the contract; an absolute
    // or protocol-relative one would move playback off the authenticated
    // host, so refuse it rather than follow it.
    let rel = resp.url;
    if !rel.starts_with('/') || rel.starts_with("//") {
        return Err(AppError::Other(
            "remote server returned a non-relative stream URL".into(),
        ));
    }
    Ok(format!("{}{}", client.base_url(), rel))
}

/// Fetch a remote artwork by hash and return it as a `data:` URL, ready to
/// drop into an `<img>` — the artwork endpoint is Bearer-only, so a bare
/// `<img src>` pointed at it would 401.
pub async fn artwork_data_url(state: &AppState, artwork_hash: &str) -> AppResult<String> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline".into()));
    }
    let client = client(state).await?;

    let response = client
        .get(&format!("/api/v2/artwork/{artwork_hash}"))
        .send()
        .await
        .map_err(|err| AppError::Other(format!("artwork fetch: {err}")))?;
    if !response.status().is_success() {
        return Err(AppError::Other(format!(
            "artwork fetch: HTTP {}",
            response.status().as_u16()
        )));
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| AppError::Other(format!("artwork body: {err}")))?;
    Ok(format!("data:{};base64,{}", mime, STANDARD.encode(&bytes)))
}
