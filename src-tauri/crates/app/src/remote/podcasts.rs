//! The server's podcasts, as `GET /api/v2/podcasts` and its neighbours answer them
//! ([RFC-009] decision 11, over the server's RFC-025 decisions 7 to 14).
//!
//! **Live, not mirrored.** Channels and episodes are in neither the journal nor the
//! snapshot, so nothing here goes through the projection or the outbound queue, and
//! nothing here touches the database: a page reads when it opens, a gesture is a
//! request, and a gesture that fails says so instead of being retried later.
//!
//! **No address reaches this side.** A manager's answer holds the feed's address, and
//! any reader's the channel's site, neither of which the wire types below declare, so
//! they are dropped at the door. What goes on to the webview cannot carry them
//! (RFC-009 decision 4), and neither can a log line.
//!
//! **A word, not a sentence.** What the server says in a refusal stays here:
//! [`failure_word`] reduces it to one word from a fixed list. Its status decides, and
//! a 404 only ever says "refused or gone": a refusal blurs to that for an account that
//! may not manage podcasts, so no role is read into one. `can_manage` says it in
//! advance.
//!
//! **Plain requests.** Idempotency headers would advertise a replay guarantee: the
//! server keeps these rows outside the operations it can recognize.
//!
//! [RFC-009]: ../../../../docs/rfcs/RFC-009-podcasts.md

use std::time::Duration;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use waveflow_core::podcast::address::parse_address;

use super::client::{segment, FailureKind, RemoteClient, RemoteFailure};
use crate::podcast::fetch::Origin;

/// The page goes on without the section, so a server that hangs does not get the
/// whole of the client's own deadline to be missed in.
const OVERVIEW_TIMEOUT: Duration = Duration::from_secs(10);

/// A position is written on the way out of an episode, from the one task that also
/// steps to the next, and at the exit. Late, it is worth less than getting on.
const POSITION_TIMEOUT: Duration = Duration::from_secs(5);

/// Words [`failure_word`] may answer with, besides those the server's feed refusal
/// may hold.
const UNREACHABLE: &str = "server_unreachable";
const SIGNED_OUT: &str = "server_signed_out";
const REFUSED: &str = "server_refused";
const CONFLICT: &str = "server_conflict";
const ADDRESS_REFUSED: &str = "address_refused";
const INVALID_FEED: &str = "invalid_feed";

/// A stored episode's `status`: the only one a stream route plays.
const STORED: &str = "completed";

/// The code of a feed the server would not subscribe to.
const FEED_REJECTED: &str = "feed_rejected";

/// A channel on the server. Declares no address: see the module doc.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ServerChannel {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    /// The cover in the server's artwork store, fetched by hash.
    pub artwork_hash: Option<String>,
    /// Why the server's last fetch of the feed failed, as a word.
    pub error: Option<String>,
    pub refreshed_at: Option<i64>,
    pub episode_count: i64,
    /// On a read of one channel, newest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episodes: Option<Vec<ServerEpisode>>,
}

/// An episode on the server, and this account's position in it. Declares no address.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ServerEpisode {
    pub id: String,
    pub channel_id: String,
    pub channel_title: String,
    pub artwork_hash: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub published_at: Option<i64>,
    pub duration_ms: Option<i64>,
    /// `new`, `downloading`, `completed`, `error` or `deleted`. Only `completed`
    /// plays: the server downloads, and never relays.
    pub status: String,
    /// Why its download failed, as a word.
    pub error: Option<String>,
    pub size: Option<i64>,
    pub content_type: Option<String>,
    /// Where this account stopped, from its bookmark. `None` is an episode never
    /// started and one finished alike: the server keeps no played state.
    pub position_ms: Option<i64>,
}

impl ServerEpisode {
    pub fn is_stored(&self) -> bool {
        self.status == STORED
    }
}

/// The answer to `GET /api/v2/podcasts`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ServerOverview {
    pub enabled: bool,
    pub can_manage: bool,
    pub channels: Vec<ServerChannel>,
}

/// Which server, and whose account on it, an episode belongs to.
///
/// Taken from the client that read it, and asked of every client after: what is
/// written goes to the account that was listening or nowhere. A profile switch fails
/// to build a client at all, and signing in elsewhere within one profile builds one
/// this no longer matches. An episode id means nothing on another server, and on
/// the same server it means somebody else's position.
#[derive(Clone, PartialEq, Eq)]
pub struct ServerScope {
    pub profile_id: i64,
    server_url: String,
    account_id: String,
}

impl std::fmt::Debug for ServerScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ServerScope {{ profile_id: {} }}", self.profile_id)
    }
}

impl ServerScope {
    /// `None` for a client whose binding names no account.
    pub fn of(client: &RemoteClient<'_>, profile_id: i64) -> Option<Self> {
        Some(Self {
            profile_id,
            server_url: client.base_url().to_owned(),
            account_id: client.account_id()?.to_owned(),
        })
    }

    pub fn holds_for(&self, client: &RemoteClient<'_>) -> bool {
        client.base_url() == self.server_url && client.account_id() == Some(&self.account_id)
    }

    /// Which networks a stream from this server may reach. Its address is one the
    /// person typed, as a feed's is, so a server on the local network may stream from
    /// there and one on the internet cannot lead back into it.
    pub(crate) fn origin(&self) -> Origin {
        parse_address(&self.server_url).map_or_else(Origin::public, |typed| Origin::of(&typed))
    }

    /// Whether the binding `conn` holds still is this one, signed in. Reads the two
    /// rows and nothing else, for what is asked before every request of a stream.
    pub async fn is_current(
        &self,
        conn: &mut sqlx::SqliteConnection,
    ) -> crate::error::AppResult<bool> {
        let Some(binding) = super::binding::read(conn).await? else {
            return Ok(false);
        };
        let account_matches = matches!(
            &binding.identity,
            super::binding::RemoteIdentity::Waveflow { account_id, .. }
                if account_id == &self.account_id
        );
        Ok(account_matches
            && binding.server_url.trim_end_matches('/') == self.server_url
            && super::tokens::read(conn).await?.is_some())
    }
}

/// The word a refused request reaches the page as, from what it answered and never
/// from what it said.
pub fn failure_word(failure: &RemoteFailure) -> String {
    if failure.code.as_deref() == Some(FEED_REJECTED) {
        return feed_word(&failure.message).to_owned();
    }
    match (failure.status, failure.kind) {
        (Some(401), _) | (_, FailureKind::Unauthorized) => SIGNED_OUT,
        (Some(409), _) => CONFLICT,
        // The address itself, before the server tried to fetch anything.
        (Some(422), _) => ADDRESS_REFUSED,
        (Some(status), FailureKind::Permanent) if status < 500 => REFUSED,
        _ => UNREACHABLE,
    }
    .to_owned()
}

/// A feed refusal's reason, when it is one the list holds. `destination_refused` is
/// the server's own: it reached a host its rules forbid.
fn feed_word(reason: &str) -> &str {
    let is_status = reason.strip_prefix("http_").is_some_and(|status| {
        status.len() == 3 && status.bytes().all(|byte| byte.is_ascii_digit())
    });
    match reason {
        "destination_refused"
        | "address_refused"
        | "too_many_redirects"
        | "too_large"
        | "unreached"
        | "invalid_feed" => reason,
        _ if is_status => reason,
        _ => INVALID_FEED,
    }
}

/// The overview, when there is a section to show: the server answered, and its
/// podcasts are on. Anything else is a page without it, and never an error on it.
pub fn visible(answer: Result<ServerOverview, RemoteFailure>) -> Option<ServerOverview> {
    match answer {
        Ok(overview) if overview.enabled => Some(overview),
        Ok(_) => None,
        Err(failure) => {
            tracing::debug!(status = ?failure.status, "podcast: no answer from the server");
            None
        }
    }
}

/// Everything the page shows before a channel is opened.
pub async fn overview(client: &RemoteClient<'_>) -> Result<ServerOverview, RemoteFailure> {
    client
        .send_json(client.get("/api/v2/podcasts").timeout(OVERVIEW_TIMEOUT))
        .await
}

/// One channel and its episodes, with this account's position in each.
pub async fn channel(client: &RemoteClient<'_>, id: &str) -> Result<ServerChannel, RemoteFailure> {
    client
        .send_json(client.get(&format!("/api/v2/podcasts/{}", segment(id)?)))
        .await
}

/// One episode, read again at the moment it is opened.
pub async fn episode(client: &RemoteClient<'_>, id: &str) -> Result<ServerEpisode, RemoteFailure> {
    client
        .send_json(client.get(&format!("/api/v2/podcast-episodes/{}", segment(id)?)))
        .await
}

/// Subscribes the server. It fetches and reads the feed before it answers, and stores
/// nothing if it could not.
pub async fn subscribe(
    client: &RemoteClient<'_>,
    feed_url: &str,
) -> Result<ServerChannel, RemoteFailure> {
    client
        .send_json(
            client
                .request(Method::POST, "/api/v2/podcasts")
                .json(&serde_json::json!({ "url": feed_url })),
        )
        .await
}

/// Asks the server to fetch every channel again, which it does in the background.
pub async fn refresh(client: &RemoteClient<'_>) -> Result<(), RemoteFailure> {
    client
        .send_ok(client.request(Method::POST, "/api/v2/podcasts/refresh"))
        .await
}

/// Unsubscribes the server, which removes the channel's files for every account.
pub async fn delete_channel(client: &RemoteClient<'_>, id: &str) -> Result<(), RemoteFailure> {
    client
        .send_ok(client.request(
            Method::DELETE,
            &format!("/api/v2/podcasts/{}", segment(id)?),
        ))
        .await
}

/// Asks the server to keep an episode, which it does in the background.
pub async fn request_download(client: &RemoteClient<'_>, id: &str) -> Result<(), RemoteFailure> {
    client
        .send_ok(client.request(
            Method::POST,
            &format!("/api/v2/podcast-episodes/{}/download", segment(id)?),
        ))
        .await
}

/// Removes an episode's file and marks it deleted on the server, which then lists it
/// to nobody, and a refresh does not bring it back.
pub async fn delete_episode(client: &RemoteClient<'_>, id: &str) -> Result<(), RemoteFailure> {
    client
        .send_ok(client.request(
            Method::DELETE,
            &format!("/api/v2/podcast-episodes/{}", segment(id)?),
        ))
        .await
}

/// Where this account stopped. The bookmark route takes an episode id as it takes a
/// track's, and replaces what was there.
pub async fn save_position(
    client: &RemoteClient<'_>,
    id: &str,
    position_ms: u64,
) -> Result<(), RemoteFailure> {
    client
        .send_ok(
            client
                .request(Method::PUT, &format!("/api/v2/bookmarks/{}", segment(id)?))
                .timeout(POSITION_TIMEOUT)
                .json(&serde_json::json!({ "position_ms": position_ms })),
        )
        .await
}

/// Forgets where this account stopped, so the episode opens from its start.
pub async fn clear_position(client: &RemoteClient<'_>, id: &str) -> Result<(), RemoteFailure> {
    client
        .send_ok(
            client
                .request(
                    Method::DELETE,
                    &format!("/api/v2/bookmarks/{}", segment(id)?),
                )
                .timeout(POSITION_TIMEOUT),
        )
        .await
}

#[cfg(test)]
mod tests;
