//! Multi-provider lyrics search used by WaveFlow.
//!
//! This crate is intentionally independent from Tauri and the database.
//! Callers provide a free-form query and receive a lyrics body plus the
//! detected format/provider.

pub(crate) mod http;
mod providers;
mod utils;

pub use providers::Provider;
pub use utils::{detect_format, LyricsFormat};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json parsing failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("provider failed: {0}")]
    Provider(String),
}

impl Error {
    /// Whether the provider itself could not be reached or refused us —
    /// a connect timeout, a 403, a 5xx — as opposed to a response that
    /// arrived but could not be read. Only the first kind says anything
    /// about the provider's health; a page that fails to parse is about
    /// that one query.
    pub fn is_transport(&self) -> bool {
        matches!(self, Error::Http(_))
    }

    /// The same error with the request URL dropped. `reqwest::Error`
    /// echoes the URL in both `Display` and `Debug`, and Musixmatch URLs
    /// carry `usertoken=` as a query parameter, so an error that is ever
    /// handed back to a caller who may log it goes through here first.
    fn redacted(self) -> Self {
        match self {
            Error::Http(err) => Error::Http(err.without_url()),
            other => other,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Plaintext,
    PreferSynced,
    SyncedOnly,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub mode: SearchMode,
    pub providers: Vec<Provider>,
    pub enhanced: bool,
    pub lang: Option<String>,
    pub genius_cookie: Option<String>,
    pub netease_cookie: Option<String>,
}

impl SearchOptions {
    pub fn synced(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            mode: SearchMode::PreferSynced,
            providers: Provider::defaults().to_vec(),
            enhanced: false,
            lang: None,
            genius_cookie: None,
            netease_cookie: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LyricsResult {
    pub content: String,
    pub format: LyricsFormat,
    pub provider: Provider,
}

/// A provider that failed to answer, and why.
#[derive(Debug)]
pub struct ProviderFailure {
    pub provider: Provider,
    /// Already stripped of the request URL — safe to log.
    pub error: Error,
}

/// Everything one [`SyncedLyricsClient::search`] learned.
///
/// A search is not a yes/no question once several providers are asked:
/// some may answer and others fail. The three cases callers have to tell
/// apart are all readable from here:
///
/// - **found** — `result` is `Some`;
/// - **miss** — `result` is `None` and at least one provider answered.
///   When `failures` is empty everyone answered and the miss is complete;
///   otherwise it is *partial*: the providers that answered had nothing,
///   and the failed ones were never heard;
/// - **no verdict** — nobody answered at all, only failures (or nothing
///   was asked).
///
/// One failing provider used to turn the whole search into an error,
/// discarding the answers of every other one. With a provider that is down
/// for good — a host that no longer accepts connections, an endpoint that
/// needs a cookie no install has — that meant the search could never
/// conclude for any track the others did not know.
#[derive(Debug, Default)]
pub struct SearchReport {
    pub result: Option<LyricsResult>,
    /// Providers that returned a response, with or without lyrics.
    pub answered: Vec<Provider>,
    pub failures: Vec<ProviderFailure>,
}

impl SearchReport {
    /// At least one provider answered, so a `None` result is a real miss.
    pub fn has_verdict(&self) -> bool {
        self.result.is_some() || !self.answered.is_empty()
    }
}

#[derive(Default)]
struct Candidate {
    synced: Option<String>,
    unsynced: Option<String>,
}

impl Candidate {
    fn update(&mut self, other: Candidate) {
        if other.synced.is_some() {
            self.synced = other.synced;
        }
        if other.unsynced.is_some() {
            self.unsynced = other.unsynced;
        }
    }

    fn preferred(&self, mode: SearchMode) -> bool {
        self.synced.is_some() || (mode == SearchMode::Plaintext && self.unsynced.is_some())
    }

    fn acceptable(&self, mode: SearchMode) -> bool {
        self.synced.is_some() || (mode != SearchMode::SyncedOnly && self.unsynced.is_some())
    }

    fn into_result(self, mode: SearchMode, provider: Provider) -> Option<LyricsResult> {
        let content = match mode {
            SearchMode::Plaintext => self
                .unsynced
                .or_else(|| self.synced.map(|s| utils::synced_to_plaintext(&s)))?,
            SearchMode::PreferSynced => self.synced.or(self.unsynced)?,
            SearchMode::SyncedOnly => self.synced?,
        };
        let format = detect_format(&content);
        Some(LyricsResult {
            content,
            format,
            provider,
        })
    }
}

pub struct SyncedLyricsClient {
    http: reqwest::Client,
}

impl SyncedLyricsClient {
    /// Build the client, returning an error instead of panicking if the HTTP
    /// stack (e.g. the TLS backend) fails to initialize.
    ///
    /// There is no infallible constructor on purpose: `reqwest::Client::new()`
    /// itself panics on a TLS/backend init failure, so callers must handle the
    /// error rather than hide it behind a panic-prone fallback.
    pub fn try_new() -> Result<Self> {
        // `http::build_client` carries the redirect host pinning + 3-hop
        // cap + connect/total timeouts. Constructing reqwest by hand
        // here would silently drop those guards.
        let http = http::build_client()?;
        Ok(Self { http })
    }

    pub async fn search(&self, options: SearchOptions) -> SearchReport {
        let mut aggregate = Candidate::default();
        let mut last_provider = None;
        let mut report = SearchReport::default();

        for provider in options.providers.iter().copied() {
            if options.lang.is_some() && provider != Provider::Musixmatch {
                continue;
            }

            let candidate = match provider {
                Provider::Musixmatch => providers::musixmatch::search(&self.http, &options).await,
                Provider::Lrclib => providers::lrclib::search(&self.http, &options.query).await,
                Provider::NetEase => {
                    providers::netease::search(
                        &self.http,
                        &options.query,
                        options.netease_cookie.as_deref(),
                    )
                    .await
                }
                Provider::Megalobiz => {
                    providers::megalobiz::search(&self.http, &options.query).await
                }
                Provider::Genius => {
                    providers::genius::search(
                        &self.http,
                        &options.query,
                        options.genius_cookie.as_deref(),
                    )
                    .await
                }
            };

            let Some(candidate) = (match candidate {
                Ok(value) => {
                    report.answered.push(provider);
                    value
                }
                Err(err) => {
                    // Redacted before it leaves this function: the
                    // request URL is dropped, because Musixmatch URLs
                    // carry `usertoken=` and the caller logs failures.
                    tracing::debug!(?provider, "lyrics provider failed");
                    report.failures.push(ProviderFailure {
                        provider,
                        error: err.redacted(),
                    });
                    None
                }
            }) else {
                continue;
            };

            // Only credit `last_provider` when this provider actually
            // contributed content — otherwise an empty Candidate from
            // a later provider would overwrite the attribution for
            // content that came from an earlier one (`into_result`
            // attaches `last_provider` to the returned LyricsResult).
            let contributed = candidate.synced.is_some() || candidate.unsynced.is_some();
            aggregate.update(candidate);
            if contributed {
                last_provider = Some(provider);
            }
            if aggregate.preferred(options.mode) {
                break;
            }
        }

        if aggregate.acceptable(options.mode) {
            report.result =
                aggregate.into_result(options.mode, last_provider.unwrap_or(Provider::Lrclib));
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_enhanced_lrc() {
        let content = "[00:01.00]<00:01.00>Hello <00:01.50>world";
        assert_eq!(detect_format(content), LyricsFormat::EnhancedLrc);
    }
}
