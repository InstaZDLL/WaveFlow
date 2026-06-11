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

    pub async fn search(&self, options: SearchOptions) -> Result<Option<LyricsResult>> {
        let mut aggregate = Candidate::default();
        let mut last_provider = None;
        let mut last_error = None;

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
                Ok(value) => value,
                Err(err) => {
                    // Sanitize the error message before tracing —
                    // `reqwest::Error`'s Debug impl echoes the request
                    // URL, and Musixmatch URLs carry `usertoken=` as a
                    // query parameter. Without this, a failed
                    // Musixmatch request would leak the token into the
                    // rolling log file. We log a static message and
                    // the provider variant; the underlying error
                    // chain is preserved through `last_error` for the
                    // structured return path.
                    tracing::debug!(?provider, "lyrics provider failed");
                    last_error = Some(err);
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

        if !aggregate.acceptable(options.mode) {
            // Distinguish "every provider genuinely had nothing" (Ok(None),
            // safe to cache as a miss) from "we never reached a verdict
            // because a provider errored" (propagate so the caller doesn't
            // poison the cache with an empty entry on a transient failure).
            return match last_error {
                Some(err) => Err(err),
                None => Ok(None),
            };
        }
        Ok(aggregate.into_result(options.mode, last_provider.unwrap_or(Provider::Lrclib)))
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
