//! What an address out of a feed may be, and the words a failure is kept as.
//!
//! The rule about which network an address may lead to needs the app's notion
//! of "public", so it lives there (`may_open`). What is decided here is what
//! no fetch of any kind should even try.

use url::Url;

/// How many redirects one fetch follows. Enclosures commonly pass through one
/// or two measurement prefixes on their way to a CDN; five leaves room for
/// that and for a feed that moved, and refuses a loop.
pub const MAX_REDIRECTS: usize = 5;

/// The longest address kept, in characters.
pub const MAX_URL_CHARS: usize = 2_048;

/// Why a fetch produced nothing. Carries nothing from the address or the
/// answer: [`Self::code`] is what is logged and stored, and a text from a
/// response or an error, which may quote the address, never is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchFailure {
    /// Not http(s), user information, no host, a private address out of a
    /// public feed, or a hop from https to http. Nothing was contacted there.
    AddressRefused,
    /// More redirects than [`MAX_REDIRECTS`], or one without a usable target.
    TooManyRedirects,
    /// The body passed its ceiling.
    TooLarge,
    /// The name did not resolve, the connection failed, or the deadline passed.
    Unreached,
    /// The host answered something other than success.
    Status(u16),
    /// A body that is not a feed.
    InvalidFeed,
    /// A body that is not audio.
    NotAudio,
    /// The download folder is at its quota.
    QuotaExceeded,
}

impl FetchFailure {
    /// The word stored in `last_error` and written to the log.
    pub fn code(self) -> String {
        match self {
            Self::AddressRefused => "address_refused".to_owned(),
            Self::TooManyRedirects => "too_many_redirects".to_owned(),
            Self::TooLarge => "too_large".to_owned(),
            Self::Unreached => "unreached".to_owned(),
            Self::Status(status) => format!("http_{status}"),
            Self::InvalidFeed => "invalid_feed".to_owned(),
            Self::NotAudio => "not_audio".to_owned(),
            Self::QuotaExceeded => "quota_exceeded".to_owned(),
        }
    }
}

/// An address typed by a person or read from a document, if it is one a fetch
/// may try: absolute http(s), a host, no user information, of a sensible
/// length. The fragment goes, since it is never sent.
pub fn parse_address(raw: &str) -> Option<Url> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().count() > MAX_URL_CHARS {
        return None;
    }
    let mut url = Url::parse(raw).ok()?;
    url.set_fragment(None);
    is_fetchable(&url).then_some(url)
}

/// The shape rules on an address already parsed, which every hop is put
/// through before it is contacted.
pub fn is_fetchable(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| !host.is_empty())
}

/// Where a redirect from `current` to `location` leads. Refused when it would
/// leave https for http, or does not name a place a fetch may try: a hop that
/// was secure must not hand its request, and the token a private feed carries
/// in it, to a clear one. One too long to be kept is refused too: a permanent
/// redirect is stored, and [`parse_address`] would not read it back.
pub fn next_hop(current: &Url, location: &str) -> Result<Url, FetchFailure> {
    let mut next = current
        .join(location.trim())
        .map_err(|_| FetchFailure::TooManyRedirects)?;
    next.set_fragment(None);
    if !is_fetchable(&next)
        || (current.scheme() == "https" && next.scheme() == "http")
        || next.as_str().chars().count() > MAX_URL_CHARS
    {
        return Err(FetchFailure::AddressRefused);
    }
    Ok(next)
}

/// What a channel is known by, stored in place of its address so a restored
/// archive, which holds no address, can still recognise it when it is
/// subscribed to again.
pub fn feed_key(url: &Url) -> String {
    blake3::hash(url.as_str().as_bytes()).to_hex().to_string()
}

/// What an episode is matched by across refreshes. A hash, because the guid
/// falls back to the enclosure's address, and a guid is often that address
/// anyway: kept as written it would be one more copy of a private token.
pub fn item_key(guid: &str) -> String {
    blake3::hash(guid.as_bytes()).to_hex().to_string()
}
