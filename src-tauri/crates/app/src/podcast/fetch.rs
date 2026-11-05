//! Every byte a podcast brings in, fetched under one set of rules.
//!
//! A feed address is typed by a person, and every address after it (a redirect,
//! an enclosure, a chapter file, a picture) is read out of a response, so each
//! one is treated as untrusted:
//!
//! - **Offline mode is checked before every request**, each hop included.
//! - **Each hop is judged before it is contacted**: its shape
//!   ([`is_fetchable`]) and where it leads ([`may_open`]). A feed whose address
//!   is public can only lead to public addresses, so a feed on the internet
//!   cannot make the player reach the user's router. A feed typed with a local
//!   address may name local ones, as a radio station may. As with the artwork
//!   downloader, no protection against DNS rebinding is attempted.
//! - **Redirects are followed by hand, at most [`MAX_REDIRECTS`]**, and never
//!   from https to http ([`next_hop`]). The client itself follows none. Permanent
//!   ones are remembered, so a feed that has moved stays followed.
//! - **Bodies are bounded while they are read**, and the whole request by a
//!   deadline, redirects included.
//! - **Nothing about a request is logged or returned but a word from a fixed
//!   list.** An address may hold a private token in its path as often as in
//!   its query, so `redact_url` is not enough here: no address goes in a log
//!   line, redacted or not, and a `reqwest::Error`, whose text quotes the
//!   address, is dropped where it is met.

use std::time::Duration;

use reqwest::header::{
    HeaderMap, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, LOCATION, RANGE,
};
use reqwest::StatusCode;
use url::Url;
use waveflow_core::podcast::address::{is_fetchable, next_hop};
use waveflow_core::podcast::{FetchFailure, MAX_FEED_BYTES, MAX_REDIRECTS};

use crate::commands::deezer::is_public_http_url;
use crate::commands::web_radio_custom::may_open;

/// How long a connection gets to open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The whole of one feed's request, redirects and body included.
const FEED_DEADLINE: Duration = Duration::from_secs(60);

/// The whole of a chapter file's, a transcript's or a picture's request.
pub(crate) const SMALL_FILE_DEADLINE: Duration = Duration::from_secs(30);

/// The longest validator kept from a response. Past this it is not one anybody
/// meant, and sending it back would only be a large header.
const MAX_VALIDATOR_CHARS: usize = 256;

const USER_AGENT: &str = concat!("WaveFlow/", env!("CARGO_PKG_VERSION"));

/// Why a fetch produced nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchError {
    /// Offline mode is on. Nothing was contacted, and this is not stored as a
    /// channel's failure: the feed did nothing wrong.
    Offline,
    Failed(FetchFailure),
}

impl From<FetchFailure> for FetchError {
    fn from(failure: FetchFailure) -> Self {
        Self::Failed(failure)
    }
}

/// Which networks a chain of addresses may reach. Decided once, from the
/// address the person typed: see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Origin {
    public: bool,
}

impl Origin {
    pub(crate) fn of(typed: &Url) -> Self {
        Self {
            public: is_public_http_url(typed.as_str()),
        }
    }

    /// The stricter of the two rules, for an address with no typed one behind it.
    pub(crate) fn public() -> Self {
        Self { public: true }
    }

    pub(crate) fn is_public(self) -> bool {
        self.public
    }
}

/// The two validators of a feed's last answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Validators {
    fn from_headers(headers: &HeaderMap) -> Self {
        let read = |name| {
            headers
                .get(name)
                .and_then(|value: &reqwest::header::HeaderValue| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.chars().count() <= MAX_VALIDATOR_CHARS)
                .map(str::to_owned)
        };
        Self {
            etag: read(ETAG),
            last_modified: read(LAST_MODIFIED),
        }
    }

    fn to_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let mut put = |name, value: &Option<String>| {
            if let Some(value) = value.as_deref().and_then(|v| v.parse().ok()) {
                headers.insert(name, value);
            }
        };
        put(IF_NONE_MATCH, &self.etag);
        put(IF_MODIFIED_SINCE, &self.last_modified);
        headers
    }
}

/// What asking for a feed came back with.
#[derive(Debug)]
pub(crate) enum FeedBody {
    /// The host says the validators still hold.
    NotModified { moved_to: Option<Url> },
    /// `base` is where the body was finally read from, which is what a relative
    /// address in it resolves against.
    Fresh {
        bytes: Vec<u8>,
        base: Url,
        validators: Validators,
        moved_to: Option<Url>,
    },
}

/// The one way out for anything a podcast names.
pub(crate) struct Fetcher {
    client: reqwest::Client,
    /// `offline::is_offline` in the app. A parameter so a test can turn it on
    /// for itself: the real switch is process-wide, and every other test in
    /// the binary would see it.
    offline: fn() -> bool,
    feed_deadline: Duration,
}

impl Fetcher {
    pub(crate) fn new() -> Result<Self, FetchError> {
        Self::with(client_builder(), crate::offline::is_offline, FEED_DEADLINE)
    }

    pub(crate) fn with(
        builder: reqwest::ClientBuilder,
        offline: fn() -> bool,
        feed_deadline: Duration,
    ) -> Result<Self, FetchError> {
        Ok(Self {
            client: builder
                .build()
                .map_err(|_| FetchError::Failed(FetchFailure::Unreached))?,
            offline,
            feed_deadline,
        })
    }

    /// How offline mode is asked about, for what reads a body long after opening
    /// it.
    pub(crate) fn offline_check(&self) -> fn() -> bool {
        self.offline
    }

    /// Whether a request made now would be refused. For work that goes on reading
    /// a body after [`Self::open`] has judged the hops.
    pub(crate) fn is_offline(&self) -> bool {
        (self.offline)()
    }

    /// Opens `url` under every rule above, following redirects by hand, and
    /// answers the response that carries the body, or a 304.
    ///
    /// Bounded by the caller's deadline, not by one of its own: a stream, a
    /// download and a feed have different budgets, and each includes every hop.
    pub(crate) async fn open(
        &self,
        url: &Url,
        origin: Origin,
        headers: HeaderMap,
    ) -> Result<reqwest::Response, FetchError> {
        self.open_tracking(url, origin, headers)
            .await
            .map(|(response, _)| response)
    }

    /// [`Self::open`], and where the address has permanently moved: the last hop
    /// of the unbroken run of 301s and 308s it began with. A temporary redirect
    /// ends the run, since what comes after it says nothing about the address.
    async fn open_tracking(
        &self,
        url: &Url,
        origin: Origin,
        headers: HeaderMap,
    ) -> Result<(reqwest::Response, Option<Url>), FetchError> {
        let mut current = url.clone();
        let mut moved_to = None;
        let mut only_permanent = true;
        for _ in 0..=MAX_REDIRECTS {
            if (self.offline)() {
                return Err(FetchError::Offline);
            }
            if !is_fetchable(&current) || !may_open(origin.public, current.as_str()) {
                return Err(FetchFailure::AddressRefused.into());
            }
            let response = self
                .client
                .get(current.clone())
                .headers(headers.clone())
                .send()
                .await
                .map_err(|_| FetchFailure::Unreached)?;
            let status = response.status();
            if status == StatusCode::NOT_MODIFIED || status.is_success() {
                return Ok((response, moved_to));
            }
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(FetchFailure::TooManyRedirects)?;
                current = next_hop(&current, location)?;
                only_permanent = only_permanent
                    && matches!(
                        status,
                        StatusCode::MOVED_PERMANENTLY | StatusCode::PERMANENT_REDIRECT
                    );
                if only_permanent {
                    moved_to = Some(current.clone());
                }
                continue;
            }
            return Err(FetchFailure::Status(status.as_u16()).into());
        }
        Err(FetchFailure::TooManyRedirects.into())
    }

    /// A feed's body, at most [`MAX_FEED_BYTES`], asked for conditionally.
    ///
    /// `origin` is the caller's, not derived from `url`: an address a feed
    /// announced must be judged by where that feed was, or a public feed could
    /// name a private one and have it judged as typed.
    pub(crate) async fn feed(
        &self,
        url: &Url,
        origin: Origin,
        validators: &Validators,
    ) -> Result<FeedBody, FetchError> {
        let within = async {
            let (response, moved_to) = self
                .open_tracking(url, origin, validators.to_headers())
                .await?;
            if response.status() == StatusCode::NOT_MODIFIED {
                return Ok(FeedBody::NotModified { moved_to });
            }
            let base = response.url().clone();
            let validators = Validators::from_headers(response.headers());
            Ok(FeedBody::Fresh {
                bytes: read_bounded(response, MAX_FEED_BYTES).await?,
                base,
                validators,
                moved_to,
            })
        };
        tokio::time::timeout(self.feed_deadline, within)
            .await
            .unwrap_or(Err(FetchFailure::Unreached.into()))
    }

    /// A small file a feed names (a picture, a chapter file, a transcript), at
    /// most `max_bytes`, reaching only what `origin` allows.
    pub(crate) async fn small_file(
        &self,
        url: &Url,
        origin: Origin,
        max_bytes: usize,
    ) -> Result<Vec<u8>, FetchError> {
        let within = async {
            let response = self.open(url, origin, HeaderMap::new()).await?;
            read_bounded(response, max_bytes).await
        };
        tokio::time::timeout(SMALL_FILE_DEADLINE, within)
            .await
            .unwrap_or(Err(FetchFailure::Unreached.into()))
    }

    /// The first `max_bytes` of a file too large to want, such as an enclosure whose
    /// tag is at its start, under the same rules. Asks for no more than that, and
    /// stops reading a host that sends the whole file anyway. Unlike
    /// [`Self::small_file`], more is not a failure: it is the case this is for.
    pub(crate) async fn head_of(
        &self,
        url: &Url,
        origin: Origin,
        max_bytes: usize,
    ) -> Result<Vec<u8>, FetchError> {
        let within = async {
            let mut headers = HeaderMap::new();
            if let Ok(range) = format!("bytes=0-{}", max_bytes.saturating_sub(1)).parse() {
                headers.insert(RANGE, range);
            }
            let mut response = self.open(url, origin, headers).await?;
            let mut bytes = Vec::new();
            while bytes.len() < max_bytes {
                let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| FetchFailure::Unreached)?
                else {
                    break;
                };
                let room = max_bytes - bytes.len();
                bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
            Ok(bytes)
        };
        tokio::time::timeout(SMALL_FILE_DEADLINE, within)
            .await
            .unwrap_or(Err(FetchFailure::Unreached.into()))
    }
}

/// The client every podcast request goes through. It follows no redirect by
/// itself, which is the point: [`Fetcher::open`] judges each one first.
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
}

/// A whole body, at most `max_bytes`. The bound is applied to the declared
/// length first and then to every chunk, so a host that lies about its length,
/// or declares none, is stopped where it passes the ceiling.
pub(crate) async fn read_bounded(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, FetchError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(FetchFailure::TooLarge.into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| FetchFailure::Unreached)?
    {
        if bytes.len() + chunk.len() > max_bytes {
            return Err(FetchFailure::TooLarge.into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
