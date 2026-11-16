//! The server's music video for a remote track.
//!
//! A clip is the video file named like a track (#1073). Locally it is found on
//! the disk at each asking ([`crate::commands::clip`]); a server track has no
//! local file to look beside, so the server does the finding, by the same rule
//! (its RFC-029), records the link at its scan and says on every track whether
//! there is one ([`super::dto::ClipField`], kept in `remote_track.has_clip`).
//!
//! Playing it is the Canvas's problem again: the file sits behind a
//! Bearer-only route, and a `<video src>` sends no header. So the server
//! answers it the same way, with a **ticket** — `POST
//! /tracks/{id}/clip-ticket` — that authorises `GET /clip-stream/{ticket}` on
//! its own, ranges included. This module is [`super::canvas`]'s shape with one
//! difference, the expiry.
//!
//! ## Offered from the mirror, minted when it opens
//!
//! Whether a track has a clip is read from the mirror ([`known`],
//! [`on_server`]), so offering one costs no request; the ticket is minted when
//! the clip opens, and again to renew it. A track reconciled with a local file
//! plays the clip beside that file first (`commands::clip`).
//!
//! ## The expiry travels with the URL
//!
//! A Canvas plays for a few seconds and is minted again with the next track. A
//! clip can outlive its ticket while it is open: a long pause, then a seek, and
//! the range request that follows meets an expired ticket. A `<video>` element
//! does not tell its caller the status of a request — a 404 at load surfaces as
//! the same `MediaError` as a container the webview cannot read — so the
//! player cannot learn from an error that the ticket ran out. It has to know
//! beforehand, and renew before it breaks: [`ClipTicket::expires_at`] is the
//! server's own answer, handed on as it is.
//!
//! ## The URL is built from our base, never the server's
//!
//! Like every ticket route, the mint answers a relative path; anything else is
//! refused ([`super::client::is_relative`]).
//!
//! ## No clip is not an error, a server out of reach is
//!
//! The server answers 404 for every refusal — no track, somebody else's, no
//! clip, a clip deleted since the scan — and that status alone resolves to
//! `None`: the clip is not there to play. Any other failure is an error: a
//! transient one, so a player that asks again later is not told the clip is
//! gone for good, and a reply that breaks the contract (another 4xx, a body
//! this build cannot read), so it is reported rather than read as "no clip".

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    error::{AppError, AppResult},
    remote::client::{is_relative, segment, RemoteClient},
    state::AppState,
};

/// A playable URL for a server track's clip, and until when it plays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipTicket {
    /// Our own base, then the server's `/api/v2/clip-stream/<ticket>`.
    pub url: String,
    /// When the ticket stops opening the clip, in Unix epoch milliseconds, as
    /// the server minted it.
    pub expires_at: i64,
}

#[derive(Deserialize)]
struct ClipTicketResponse {
    /// Relative, e.g. `/api/v2/clip-stream/<ticket>`.
    url: String,
    expires_at: i64,
}

/// What the mirror knows of `remote_track_id`'s clip: `Some(true)` when the
/// server named one, `Some(false)` when it said `null`, and `None` when it said
/// nothing (a server older than the field, a row not read again since this
/// version) or the track is not mirrored at all.
pub async fn known(pool: &SqlitePool, remote_track_id: &str) -> AppResult<Option<bool>> {
    let has_clip: Option<Option<i64>> =
        sqlx::query_scalar("SELECT has_clip FROM remote_track WHERE remote_id = ?")
            .bind(remote_track_id)
            .fetch_optional(pool)
            .await?;
    Ok(has_clip.flatten().map(|flag| flag == 1))
}

/// Whether the server has a clip for `remote_track_id`, to offer it.
///
/// Read from the mirror, so a menu asks nothing of the network. When the
/// mirror knows nothing and `probe` is set, the server is asked: the mint
/// answers the question a missing `has_clip` leaves open, and the ticket is
/// thrown away, since the one that plays is minted when the clip opens. The
/// now-playing track probes, a menu does not: one request per track that
/// starts is the price of a row the mirror has not read again, one per menu
/// opened would not be.
///
/// Offline, a server clip cannot play, so it is not offered.
pub async fn on_server(
    state: &AppState,
    pool: &SqlitePool,
    remote_track_id: &str,
    probe: bool,
) -> AppResult<bool> {
    if crate::offline::is_offline() {
        return Ok(false);
    }
    match known(pool, remote_track_id).await? {
        Some(has_clip) => Ok(has_clip),
        None if probe => Ok(ticket(state, remote_track_id).await?.is_some()),
        None => Ok(false),
    }
}

/// A ticket for the clip of the server track `remote_track_id`, or `None` when
/// it has none.
///
/// Never cached: a ticket is a permission valid for about an hour, not a
/// property of the track. Asked again to renew one.
pub async fn ticket(state: &AppState, remote_track_id: &str) -> AppResult<Option<ClipTicket>> {
    // An error and not `None`: offline is a state the user toggles back, and
    // the clip is still on the server.
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline".into()));
    }
    let Some(client) = RemoteClient::try_build(state).await? else {
        // Not bound to a server at all: the track the caller believes remote
        // has nowhere to have a clip.
        return Ok(None);
    };
    mint(&client, remote_track_id).await
}

/// Ask `client`'s server for the ticket. Split from [`ticket`] so a test can
/// point it at a server of its own.
pub(crate) async fn mint(
    client: &RemoteClient<'_>,
    remote_track_id: &str,
) -> AppResult<Option<ClipTicket>> {
    // An identifier that cannot be a path segment names no track the server
    // could have a clip for.
    let Ok(track) = segment(remote_track_id) else {
        return Ok(None);
    };
    let path = format!("/api/v2/tracks/{track}/clip-ticket");
    let response: ClipTicketResponse = match client
        .send_json(client.request(reqwest::Method::POST, &path))
        .await
    {
        Ok(response) => response,
        // 404 is the server's answer to every refusal: there is no clip to
        // play, and the player ends the visit on it. Only that status means
        // so. Everything else is an error: a transient failure, but also the
        // other permanent ones (another 4xx, or a 200 whose body this build
        // cannot read), which break the contract rather than say "no clip".
        Err(failure) if failure.status == Some(404) => return Ok(None),
        Err(failure) => {
            return Err(AppError::Other(format!("clip ticket: {}", failure.message)));
        }
    };
    if !is_relative(&response.url) {
        return Err(AppError::Other(
            "remote server returned a non-relative clip URL".into(),
        ));
    }
    Ok(Some(ClipTicket {
        url: format!("{}{}", client.base_url(), response.url),
        expires_at: response.expires_at,
    }))
}

#[cfg(test)]
mod tests;
