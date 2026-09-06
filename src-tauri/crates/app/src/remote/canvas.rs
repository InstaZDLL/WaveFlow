//! The server's Canvas for a remote track.
//!
//! A Canvas is a short looping clip the now-playing view paints behind the
//! cover. Locally it is a file on disk; on the server it is a blob behind a
//! Bearer-only endpoint, which a `<video src>` cannot reach — the webview
//! sends no Authorization header.
//!
//! The server answers that the same way it answers it for audio: a **ticket**,
//! sealed and time-limited, which authorises one ticketed endpoint on its own.
//! So this module is [`super::stream`]'s shape applied to a second kind of
//! media, and it keeps the same rule about where the URL comes from.
//!
//! ## The URL is built from our base, never the server's
//!
//! `POST /tracks/{id}/canvas-ticket` answers a **relative** path. We prepend
//! the binding's own `base_url` and refuse anything absolute or
//! protocol-relative: an absolute URL from the server would point the webview
//! at a host the user never authenticated against, and a `<video>` element
//! will happily load it.
//!
//! ## A track without a Canvas is not an error
//!
//! Most tracks have none, and the server says so with a 404. That is an
//! ordinary answer here, not a failure: it resolves to `None` and the
//! now-playing view falls further down its backdrop precedence. Only a
//! genuine failure — unreachable, unauthorised, malformed — is an error, and
//! even then the caller treats it as "no Canvas" rather than breaking
//! playback.

use serde::Deserialize;

use crate::{
    error::{AppError, AppResult},
    remote::client::{FailureKind, RemoteClient},
    state::AppState,
};

#[derive(Deserialize)]
struct CanvasTicketResponse {
    /// Relative, e.g. `/api/v2/canvas-stream/<ticket>`.
    url: String,
}

/// A playable URL for a server track's Canvas, or `None` when it has none.
///
/// The ticket expires (about an hour), so the answer is deliberately not
/// cached anywhere durable: it describes a permission valid now, not a
/// property of the track.
pub async fn ticket_url(state: &AppState, remote_track_id: &str) -> AppResult<Option<String>> {
    if crate::offline::is_offline() {
        return Ok(None);
    }
    let Some(client) = RemoteClient::try_build(state).await? else {
        // Not bound to a server at all. Not an error: the caller is asking
        // about a track it believes is remote, and the honest answer when
        // there is no server is that there is no Canvas.
        return Ok(None);
    };

    let path = format!("/api/v2/tracks/{remote_track_id}/canvas-ticket");
    let response: CanvasTicketResponse = match client
        .send_json(client.request(reqwest::Method::POST, &path))
        .await
    {
        Ok(response) => response,
        // 404 is the answer for "this track has no Canvas", and it arrives
        // classified as permanent. Anything else permanent — a track that is
        // not ours, a malformed id — means the same thing to the caller:
        // nothing to paint. Transient failures are reported, so a server
        // that is merely unreachable does not read as "no Canvas forever" to
        // a caller that decides to remember the answer.
        Err(failure) if failure.kind == FailureKind::Permanent => return Ok(None),
        Err(failure) => {
            return Err(AppError::Other(format!(
                "canvas ticket: {}",
                failure.message
            )))
        }
    };

    Ok(Some(absolute(&client, &response.url)?))
}

/// Whether the server sent a path we may append to our own base.
///
/// Its own function so the test exercises the rule production uses rather
/// than a copy of it: a predicate restated in a test is one that can drift
/// from the code while staying green.
fn is_relative(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//")
}

/// Prepend our own base to the server's relative path, refusing anything
/// that is not one.
fn absolute(client: &RemoteClient<'_>, relative: &str) -> AppResult<String> {
    if !is_relative(relative) {
        return Err(AppError::Other(
            "remote server returned a non-relative canvas URL".into(),
        ));
    }
    Ok(format!("{}{}", client.base_url(), relative))
}

#[cfg(test)]
mod tests {
    use super::is_relative;

    #[test]
    fn only_a_relative_path_is_accepted() {
        assert!(is_relative("/api/v2/canvas-stream/abc"));

        // The ones that matter: each would move the webview off the host the
        // user authenticated against, and a `<video>` element would load them
        // without a murmur.
        assert!(!is_relative("//evil.example/canvas"));
        assert!(!is_relative("https://evil.example/canvas"));
        assert!(!is_relative("http://evil.example/canvas"));
        assert!(!is_relative("javascript:alert(1)"));
        assert!(!is_relative("api/v2/canvas-stream/abc"));
        assert!(!is_relative(""));
    }
}
