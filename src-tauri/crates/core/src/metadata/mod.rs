//! HTTP clients for the third-party metadata sources WaveFlow
//! enriches its local library with.
//!
//! Every client here is a plain `reqwest::Client` wrapper — no Tauri,
//! no database, no filesystem. They are designed to be reusable from
//! both the desktop app (`crates/app`) and `waveflow-server` without any
//! glue.
//!
//! The three enrichment clients — Last.fm, TheAudioDB and Deezer, the ones
//! `waveflow-server` reads through — have two constructors (ListenBrainz,
//! which only the desktop uses, has `new()` alone, built by
//! [`default_http`] like the others' `new()`). `new()` builds its own HTTP client
//! and aims at the provider's public address, which is what the desktop
//! wants. `with_http` takes both from the caller, because a server has
//! rules the desktop does not: it follows no redirect, and its tests
//! point every outbound call at a listener of their own.

pub mod album_match;
pub mod deezer;
pub mod lastfm;
pub mod listenbrainz;
pub mod lrclib;
pub mod name_match;
pub mod theaudiodb;

/// The most a provider's answer may weigh before it is refused unread.
///
/// The largest real answer is a Last.fm biography or a page of a Deezer
/// tracklist, a few tens of kilobytes. The ceiling is two orders of
/// magnitude above that, so it never trims a genuine answer and still
/// stops a response that would otherwise be read into memory whole.
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Why a read-only lookup produced no answer — as opposed to the
/// provider answering that it knows nothing, which is `Ok(None)` or an
/// empty list and is worth remembering. Every variant here is worth
/// asking again.
#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    /// Built through `From`, which strips the URL — see [`redact`].
    /// Its message carries the cause chain — the refusal, the DNS
    /// failure, the timeout — see [`crate::error::describe_http_error`].
    #[error("request failed: {}", crate::error::describe_http_error(.0))]
    Http(reqwest::Error),
    /// The provider answered, and its answer was a refusal: a bad key, a
    /// suspended one, a quota spent. Not "nothing matched" — reading it
    /// as that would remember a throttled minute as an artist with no
    /// biography.
    #[error("the provider refused the request (code {code})")]
    Refused { code: i64 },
    /// A failure status with no body the provider's own error format
    /// could be read from.
    #[error("the provider answered HTTP {status}")]
    Status { status: u16 },
    #[error("the answer exceeds {limit} bytes")]
    TooLarge { limit: usize },
    #[error("the answer is not the expected JSON: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Why a body could not be read: the only two failures [`read_bounded`]
/// can produce, kept apart from [`LookupError`] so a client with its own
/// error type converts them without a case that cannot happen.
#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    /// Built through `From`, which strips the URL — see [`redact`].
    /// Its message carries the cause chain — the refusal, the DNS
    /// failure, the timeout — see [`crate::error::describe_http_error`].
    #[error("request failed: {}", crate::error::describe_http_error(.0))]
    Http(reqwest::Error),
    #[error("the answer exceeds {limit} bytes")]
    TooLarge { limit: usize },
}

/// A transport error without the URL it was raised on.
///
/// `reqwest` prints the request URL in both `Display` and `Debug`, and
/// these URLs carry the caller's key — in Last.fm's query string, in
/// TheAudioDB's path. Every caller logs the error it gets, so an error
/// that kept its URL would write the key into the log on the first
/// network hiccup.
fn redact(err: reqwest::Error) -> reqwest::Error {
    err.without_url()
}

impl From<reqwest::Error> for LookupError {
    fn from(err: reqwest::Error) -> Self {
        LookupError::Http(redact(err))
    }
}

impl From<reqwest::Error> for BodyError {
    fn from(err: reqwest::Error) -> Self {
        BodyError::Http(redact(err))
    }
}

impl From<BodyError> for LookupError {
    fn from(err: BodyError) -> Self {
        match err {
            BodyError::Http(err) => LookupError::Http(err),
            BodyError::TooLarge { limit } => LookupError::TooLarge { limit },
        }
    }
}

/// A response body read whole, unless it outgrows `limit`.
///
/// `Content-Length` is checked first so an announced giant is refused
/// before a byte of it is read; the running count covers a response that
/// announces nothing, or lies.
pub(crate) async fn read_bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, BodyError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(BodyError::TooLarge { limit });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Err(BodyError::TooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Why a client that carries a key refused the address it was given.
#[derive(Debug, thiserror::Error)]
pub enum BaseUrlError {
    #[error("not an absolute URL")]
    Invalid,
    /// The key — in Last.fm's query string, in TheAudioDB's path — would
    /// cross the network readable by anyone on the way.
    #[error("a key would travel in clear to {host}; the address must be https")]
    Plaintext { host: String },
}

/// `base_url`, if a key can be sent to it.
///
/// `https` always; plain `http` only to this machine, which is where a
/// test's listener or a developer's local stand-in lives and where there
/// is no wire to read the key from. Every other plaintext address is
/// refused before a request exists to carry the key.
pub(crate) fn require_secure(base_url: String) -> Result<String, BaseUrlError> {
    let parsed = reqwest::Url::parse(&base_url).map_err(|_| BaseUrlError::Invalid)?;
    let host = parsed.host_str().ok_or(BaseUrlError::Invalid)?;
    // An IPv6 host comes back bracketed, `[::1]`.
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    match parsed.scheme() {
        "https" => Ok(base_url),
        "http" if loopback => Ok(base_url),
        _ => Err(BaseUrlError::Plaintext {
            host: host.to_owned(),
        }),
    }
}

/// How many redirects a `new()` client follows, as `reqwest` did by default.
const MAX_REDIRECTS: usize = 10;

/// The redirect rule of a `new()` client: follow, but never out of the
/// origin first asked.
///
/// The origin includes the scheme, so this also forbids falling back to
/// plaintext. It matters for the signed Last.fm calls above all: on a
/// `307` or `308`, `reqwest` sends the body again — a form carrying a
/// session key or a password — to wherever the redirect points.
/// `require_secure` vets the address a caller chose; this vets the ones
/// the provider chooses afterwards.
fn same_origin_only() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let first = attempt.previous().first().map(reqwest::Url::origin);
        if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if first.as_ref() == Some(&attempt.url().origin()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// Builds the client `new()` constructors use: the desktop's timeout and
/// user agent, as each module used to build for itself, and redirects
/// kept inside the origin.
pub(crate) fn default_http(user_agent: &str, timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .redirect(same_origin_only())
        .build()
        .expect("failed to build reqwest client")
}

/// A canned HTTP listener for the tests in this module's children: it
/// answers every connection with the same response, verbatim, and keeps
/// the head of each request it received — which is how a test proves a
/// redirect was *not* followed, or that a key went where it belongs.
#[cfg(test)]
pub(crate) mod test_server;

#[cfg(test)]
mod tests;
