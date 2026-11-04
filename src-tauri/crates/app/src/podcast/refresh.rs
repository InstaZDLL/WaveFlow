//! Subscribing, refreshing and importing: where the network meets the database.
//!
//! A subscription fetches the feed before anything is stored, so a typo leaves
//! nothing behind. Nothing here holds a database lease across a network wait: a
//! lease held through a slow host would make a profile switch wait out its
//! drain timeout, so the pool is leased again around each write, pinned to the
//! profile the work began for, and a switch stops the work instead of letting
//! it write into the wrong profile.
//!
//! Refreshing is a background writer, so it follows the shape every such writer
//! does: it parks behind a running scan, and retries a write that meets a busy
//! database.
//!
//! Every failure is logged and stored as a word from `FetchFailure::code`, with
//! the channel's id at most. See [`super::fetch`] for why.

use std::future::Future;
use std::ops::Deref;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use sqlx::SqlitePool;
use url::Url;
use waveflow_core::podcast::address::{feed_key, next_hop, parse_address};
use waveflow_core::podcast::feed::parse_feed;
use waveflow_core::podcast::opml::OpmlEntry;
use waveflow_core::podcast::{Feed, FetchFailure};

use super::fetch::{FeedBody, FetchError, Fetcher, Origin, Validators};
use super::store::{self, ChannelSource};
use crate::error::AppError;

/// The largest channel picture fetched: what the shared artwork cache takes.
const MAX_COVER_BYTES: usize = waveflow_core::artwork::metadata::MAX_BYTES;

/// How often a pass parked behind a library scan looks again.
const SCAN_WAIT_POLL: Duration = Duration::from_millis(500);

/// Attempts at a write that keeps meeting a busy database.
const MAX_WRITE_ATTEMPTS: u32 = 5;

/// Why something a person asked for did not happen. What reaches the frontend
/// is the word after `podcast: `, never a text from a response.
#[derive(Debug)]
pub(crate) enum PodcastError {
    /// `podcasts.enabled` is off. Nothing was contacted.
    Disabled,
    /// Offline mode is on. Nothing was contacted.
    Offline,
    Failed(FetchFailure),
    /// The active profile changed while this was running. Nothing more was
    /// written.
    ProfileChanged,
    Database(sqlx::Error),
    Other(AppError),
}

impl From<sqlx::Error> for PodcastError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

impl From<FetchError> for PodcastError {
    fn from(err: FetchError) -> Self {
        match err {
            FetchError::Offline => Self::Offline,
            FetchError::Failed(failure) => Self::Failed(failure),
        }
    }
}

impl From<PodcastError> for AppError {
    fn from(err: PodcastError) -> Self {
        match err {
            PodcastError::Disabled => AppError::Other("podcast: disabled".into()),
            PodcastError::Offline => AppError::Other("podcast: offline".into()),
            PodcastError::Failed(failure) => {
                AppError::Other(format!("podcast: {}", failure.code()))
            }
            PodcastError::ProfileChanged => AppError::Other("podcast: profile_changed".into()),
            PodcastError::Database(err) => AppError::Database(err),
            PodcastError::Other(err) => err,
        }
    }
}

/// Where the pool comes from each time it is needed.
pub(crate) trait Leases: Sync {
    type Lease: Deref<Target = SqlitePool> + Send;

    /// The pool of the profile this work began for, or
    /// [`PodcastError::ProfileChanged`] once it is no longer active.
    fn lease(&self) -> impl Future<Output = Result<Self::Lease, PodcastError>> + Send;
}

/// What a person asked to subscribe to turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Subscribed {
    /// A new channel, or one a restored archive left without its address.
    Added(i64),
    /// Already followed. Nothing was fetched.
    Already(i64),
}

impl Subscribed {
    pub(crate) fn channel_id(self) -> i64 {
        match self {
            Self::Added(id) | Self::Already(id) => id,
        }
    }
}

/// How a pass over several channels ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassOutcome {
    /// Every channel was asked.
    Done,
    Cancelled,
    /// Offline mode was on, or was turned on during the pass.
    Offline,
    /// The switch was off, or was turned off during the pass.
    Disabled,
    /// The active profile changed; the pass stopped writing to the old one.
    ProfileChanged,
    /// Another pass is running, and this one did nothing.
    AlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RefreshSummary {
    pub outcome: PassOutcome,
    /// Channels whose feed was read and stored.
    pub refreshed: u32,
    /// Channels whose host said nothing had changed.
    pub unchanged: u32,
    /// Channels that failed. Each holds its word in `last_error`.
    pub failed: u32,
}

impl RefreshSummary {
    pub(crate) fn ended(outcome: PassOutcome) -> Self {
        Self {
            outcome,
            refreshed: 0,
            unchanged: 0,
            failed: 0,
        }
    }
}

/// One feed of a list that could not be subscribed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpmlFailure {
    /// The title the list gave it. Its address is deliberately not here, since
    /// the page showing this is what ends up in a screenshot.
    pub title: Option<String>,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpmlImportReport {
    pub outcome: PassOutcome,
    pub added: u32,
    pub already: u32,
    pub failed: Vec<OpmlFailure>,
    /// How many feeds the list held, so a pass stopped short can say so.
    pub total: u32,
}

/// What a pass reports along the way, without knowing about Tauri.
pub(crate) trait PassObserver: Sync {
    fn cancelled(&self) -> bool;
    fn progress(&self, done: u64, total: u64);
    fn channel_changed(&self, channel_id: i64);
}

async fn enabled_or_refuse<L: Leases>(leases: &L) -> Result<(), PodcastError> {
    let pool = leases.lease().await?;
    if store::is_enabled(&pool).await? {
        Ok(())
    } else {
        Err(PodcastError::Disabled)
    }
}

/// Fetches the feed at `raw` and, if it is one, stores it. Fetched before
/// anything is stored, so what is not a feed leaves nothing behind.
pub(crate) async fn subscribe<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    raw: &str,
    artwork_dir: &Path,
) -> Result<Subscribed, PodcastError> {
    enabled_or_refuse(leases).await?;
    let url = parse_address(raw).ok_or(PodcastError::Failed(FetchFailure::AddressRefused))?;
    {
        let pool = leases.lease().await?;
        if let Some((id, true)) = store::find_by_key(&pool, &feed_key(&url)).await? {
            return Ok(Subscribed::Already(id));
        }
    }
    // Nothing conditional was asked, so "unchanged" is a host talking nonsense.
    let FeedRead::Fresh {
        address,
        feed,
        validators,
    } = read_address(fetcher, &url, &Validators::default()).await?
    else {
        return Err(PodcastError::Failed(FetchFailure::InvalidFeed));
    };
    let key = feed_key(&address);
    let image_url = feed.image_url.clone();
    let id = {
        let pool = leases.lease().await?;
        // What was typed may have led to a feed already followed.
        if let Some((id, true)) = store::find_by_key(&pool, &key).await? {
            return Ok(Subscribed::Already(id));
        }
        with_busy_retry(|| store::apply_feed(&pool, &key, address.as_str(), &feed, &validators))
            .await?
    };
    update_artwork(
        leases,
        fetcher,
        &address,
        id,
        image_url.as_deref(),
        None,
        artwork_dir,
    )
    .await;
    Ok(Subscribed::Added(id))
}

/// What asking an address came to.
enum FeedRead {
    Unchanged {
        /// Where the address leads now, which may not be where it was.
        address: Url,
    },
    Fresh {
        address: Url,
        feed: Box<Feed>,
        validators: Validators,
    },
}

/// The feed at `url`, following it when it says it has moved, by a permanent
/// redirect or by `itunes:new-feed-url`.
async fn read_address(
    fetcher: &Fetcher,
    url: &Url,
    validators: &Validators,
) -> Result<FeedRead, PodcastError> {
    let origin = Origin::of(url);
    match fetcher.feed(url, origin, validators).await? {
        FeedBody::NotModified { moved_to } => Ok(FeedRead::Unchanged {
            address: moved_to.unwrap_or_else(|| url.clone()),
        }),
        FeedBody::Fresh {
            bytes,
            base,
            validators,
            moved_to,
        } => {
            let address = moved_to.unwrap_or_else(|| url.clone());
            let feed = read_feed(&bytes, &base)?;
            if let Some(announced) = feed.new_feed_url.as_deref() {
                if let Some(read) = read_announced(fetcher, &address, origin, announced).await? {
                    return Ok(read);
                }
            }
            Ok(FeedRead::Fresh {
                address,
                feed,
                validators,
            })
        }
    }
}

/// The feed at an address a feed announced, or `None` to stay where it is.
///
/// It is put through the rules of a redirect, and judged by where the announcing
/// feed was, so a public feed cannot announce the user's router. It is read before
/// it is believed: a typo in someone's feed must not cost its subscribers the
/// show. The feed found there is not asked in turn; if it has moved again, the
/// next refresh finds out.
async fn read_announced(
    fetcher: &Fetcher,
    address: &Url,
    origin: Origin,
    announced: &str,
) -> Result<Option<FeedRead>, PodcastError> {
    let Ok(next) = next_hop(address, announced) else {
        tracing::debug!("podcast feed announced an address that is refused");
        return Ok(None);
    };
    if next == *address {
        return Ok(None);
    }
    match fetcher.feed(&next, origin, &Validators::default()).await {
        Ok(FeedBody::Fresh {
            bytes,
            base,
            validators,
            moved_to,
        }) => Ok(read_feed(&bytes, &base).ok().map(|feed| FeedRead::Fresh {
            address: moved_to.unwrap_or(next),
            feed,
            validators,
        })),
        Ok(FeedBody::NotModified { .. }) => Ok(None),
        Err(FetchError::Offline) => Err(PodcastError::Offline),
        Err(FetchError::Failed(failure)) => {
            tracing::debug!(
                code = failure.code(),
                "podcast feed announced an address that did not answer"
            );
            Ok(None)
        }
    }
}

fn read_feed(bytes: &[u8], base: &Url) -> Result<Box<Feed>, PodcastError> {
    parse_feed(bytes, base)
        .map(Box::new)
        .map_err(|_| PodcastError::Failed(FetchFailure::InvalidFeed))
}

/// Asks every channel that has an address, or only `only`, for its feed.
///
/// A pass over them all is what the six hours count from, so it stamps the
/// profile. One the user stopped does too, or the timer would start it again
/// straight away; one that reached nobody, because of offline mode, does not.
pub(crate) async fn refresh_pass<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    only: Option<i64>,
    artwork_dir: &Path,
    observer: &impl PassObserver,
) -> Result<RefreshSummary, PodcastError> {
    let summary = ask_channels(leases, fetcher, only, artwork_dir, observer).await?;
    if only.is_none() && matches!(summary.outcome, PassOutcome::Done | PassOutcome::Cancelled) {
        let pool = leases.lease().await?;
        with_busy_retry(|| store::stamp_last_check(&pool)).await?;
    }
    Ok(summary)
}

async fn ask_channels<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    only: Option<i64>,
    artwork_dir: &Path,
    observer: &impl PassObserver,
) -> Result<RefreshSummary, PodcastError> {
    let sources = {
        let pool = leases.lease().await?;
        if !store::is_enabled(&pool).await? {
            return Ok(RefreshSummary::ended(PassOutcome::Disabled));
        }
        match only {
            Some(id) => store::source_of(&pool, id).await?.into_iter().collect(),
            None => store::refreshable(&pool).await?,
        }
    };
    let total = sources.len() as u64;
    observer.progress(0, total);
    let mut summary = RefreshSummary::ended(PassOutcome::Done);
    for (index, source) in sources.into_iter().enumerate() {
        wait_out_library_scan(observer).await;
        if observer.cancelled() {
            summary.outcome = PassOutcome::Cancelled;
            return Ok(summary);
        }
        match refresh_channel(leases, fetcher, &source, artwork_dir).await {
            Ok(Refreshed::Changed) => {
                summary.refreshed += 1;
                observer.channel_changed(source.id);
            }
            Ok(Refreshed::Unchanged) => summary.unchanged += 1,
            Ok(Refreshed::Gone) => {}
            Err(PodcastError::Failed(failure)) => {
                summary.failed += 1;
                let code = failure.code();
                tracing::info!(channel_id = source.id, code, "podcast refresh failed");
                let pool = leases.lease().await;
                match pool {
                    Ok(pool) => {
                        with_busy_retry(|| store::record_failure(&pool, source.id, &code)).await?;
                    }
                    Err(PodcastError::ProfileChanged) => {
                        summary.outcome = PassOutcome::ProfileChanged;
                        return Ok(summary);
                    }
                    Err(err) => return Err(err),
                }
                observer.channel_changed(source.id);
            }
            Err(PodcastError::Offline) => {
                summary.outcome = PassOutcome::Offline;
                return Ok(summary);
            }
            Err(PodcastError::Disabled) => {
                summary.outcome = PassOutcome::Disabled;
                return Ok(summary);
            }
            Err(PodcastError::ProfileChanged) => {
                summary.outcome = PassOutcome::ProfileChanged;
                return Ok(summary);
            }
            Err(err) => return Err(err),
        }
        observer.progress(index as u64 + 1, total);
    }
    Ok(summary)
}

enum Refreshed {
    Changed,
    Unchanged,
    /// Unsubscribed from while its feed was being read. Nothing was written.
    Gone,
}

async fn refresh_channel<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    source: &ChannelSource,
    artwork_dir: &Path,
) -> Result<Refreshed, PodcastError> {
    // Read per channel, not once per pass: turning the switch off stops the
    // requests here and now, not when a pass over a hundred feeds has ended.
    enabled_or_refuse(leases).await?;
    // A stored address that no longer parses was stored by something else, and
    // is refused the way a typed one would be.
    let url = parse_address(&source.feed_url)
        .ok_or(PodcastError::Failed(FetchFailure::AddressRefused))?;
    let validators = Validators {
        etag: source.etag.clone(),
        last_modified: source.last_modified.clone(),
    };
    let read = read_address(fetcher, &url, &validators).await?;
    let (FeedRead::Unchanged { address } | FeedRead::Fresh { address, .. }) = &read;
    let current = settle_address(leases, source.id, &url, address).await?;
    // The picture needs fetching when its address moved, or when the cache lost
    // it: a cleared cache must not leave a channel without one for as long as
    // its feed does not change.
    let (refreshed, image_url, known) = match read {
        FeedRead::Unchanged { .. } => {
            let pool = leases.lease().await?;
            with_busy_retry(|| store::record_unchanged(&pool, source.id)).await?;
            (
                Refreshed::Unchanged,
                source.image_url.clone(),
                source.artwork_hash.as_deref(),
            )
        }
        FeedRead::Fresh {
            feed, validators, ..
        } => {
            let image_url = feed.image_url.clone();
            let still_followed = {
                let pool = leases.lease().await?;
                with_busy_retry(|| store::refresh_feed(&pool, source.id, &feed, &validators))
                    .await?
            };
            if !still_followed {
                return Ok(Refreshed::Gone);
            }
            let known = source
                .artwork_hash
                .as_deref()
                .filter(|_| image_url == source.image_url);
            (Refreshed::Changed, image_url, known)
        }
    };
    update_artwork(
        leases,
        fetcher,
        &current,
        source.id,
        image_url.as_deref(),
        known,
        artwork_dir,
    )
    .await;
    Ok(refreshed)
}

/// The address a channel goes on with: `address` when it has moved there, `url`
/// when it has not, or when another channel already follows the new one.
async fn settle_address<L: Leases>(
    leases: &L,
    channel_id: i64,
    url: &Url,
    address: &Url,
) -> Result<Url, PodcastError> {
    if address == url {
        return Ok(url.clone());
    }
    let pool = leases.lease().await?;
    let key = feed_key(address);
    let moved =
        with_busy_retry(|| store::move_channel(&pool, channel_id, &key, address.as_str())).await?;
    tracing::info!(channel_id, moved, "podcast feed has moved");
    Ok(if moved { address.clone() } else { url.clone() })
}

/// Brings a channel's picture into the shared cache, so opening the page
/// contacts nobody. `known` is the hash already recorded: a file still on disk
/// under it needs nothing.
///
/// Best effort: a channel without its picture is still a channel, so a failure
/// is a line in the log and the next refresh tries again.
async fn update_artwork<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    feed_url: &Url,
    channel_id: i64,
    image_url: Option<&str>,
    known: Option<&str>,
    artwork_dir: &Path,
) {
    let Some(image) = image_url.and_then(parse_address) else {
        return;
    };
    if known.is_some_and(|hash| {
        waveflow_core::artwork::metadata::existing_path(artwork_dir, hash).is_some()
    }) {
        return;
    }
    let bytes = match fetcher
        .small_file(&image, Origin::of(feed_url), MAX_COVER_BYTES)
        .await
    {
        Ok(bytes) => bytes,
        Err(FetchError::Failed(failure)) => {
            tracing::debug!(
                channel_id,
                code = failure.code(),
                "podcast artwork not fetched"
            );
            return;
        }
        Err(FetchError::Offline) => return,
    };
    if !waveflow_core::podcast::accepts_cover(&bytes) {
        tracing::debug!(channel_id, "podcast artwork refused: not a usable image");
        return;
    }
    let Some(hash) = waveflow_core::artwork::metadata::store_bytes(&bytes, artwork_dir, true)
    else {
        return;
    };
    let Ok(pool) = leases.lease().await else {
        return;
    };
    if let Err(err) = with_busy_retry(|| store::record_artwork(&pool, channel_id, &hash)).await {
        tracing::warn!(channel_id, %err, "podcast artwork not recorded");
    }
}

/// Subscribes to each feed of a list in turn, public addresses only. One that fails
/// is reported and the next is tried, so one dead feed does not lose the rest of
/// someone's list.
pub(crate) async fn import_pass<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    entries: Vec<OpmlEntry>,
    artwork_dir: &Path,
    observer: &impl PassObserver,
) -> Result<OpmlImportReport, PodcastError> {
    enabled_or_refuse(leases).await?;
    let total = entries.len();
    let mut report = OpmlImportReport {
        outcome: PassOutcome::Done,
        added: 0,
        already: 0,
        failed: Vec::new(),
        total: total as u32,
    };
    observer.progress(0, total as u64);
    for (index, entry) in entries.into_iter().enumerate() {
        wait_out_library_scan(observer).await;
        if observer.cancelled() {
            report.outcome = PassOutcome::Cancelled;
            return Ok(report);
        }
        // A list is a file someone may have been handed, not an address typed by
        // the person it runs for, so it does not get what typing gives: one naming
        // the local network would make this a probe of it, each line's word saying
        // what answered. Refused before contact, as an address out of a response.
        let local = parse_address(&entry.feed_url).is_some_and(|url| !Origin::of(&url).is_public());
        if local {
            report.failed.push(OpmlFailure {
                title: entry.title,
                code: FetchFailure::AddressRefused.code(),
            });
            observer.progress(index as u64 + 1, total as u64);
            continue;
        }
        match subscribe(leases, fetcher, &entry.feed_url, artwork_dir).await {
            Ok(Subscribed::Added(id)) => {
                report.added += 1;
                observer.channel_changed(id);
            }
            Ok(Subscribed::Already(_)) => report.already += 1,
            Err(PodcastError::Failed(failure)) => {
                let code = failure.code();
                tracing::info!(code, "podcast import: one feed refused");
                report.failed.push(OpmlFailure {
                    title: entry.title,
                    code,
                });
            }
            Err(PodcastError::Offline) => {
                report.outcome = PassOutcome::Offline;
                return Ok(report);
            }
            Err(PodcastError::Disabled) => {
                report.outcome = PassOutcome::Disabled;
                return Ok(report);
            }
            Err(PodcastError::ProfileChanged) => {
                report.outcome = PassOutcome::ProfileChanged;
                return Ok(report);
            }
            Err(err) => return Err(err),
        }
        observer.progress(index as u64 + 1, total as u64);
    }
    Ok(report)
}

async fn wait_out_library_scan(observer: &impl PassObserver) {
    while crate::commands::scan::scan_in_flight() && !observer.cancelled() {
        tokio::time::sleep(SCAN_WAIT_POLL).await;
    }
}

/// Runs a write again while it meets a busy database, with a short backoff: a
/// library scan or a play being recorded can hold the single writer for a
/// moment, and a lost write here would be a feed fetched for nothing.
async fn with_busy_retry<T, F, Fut>(mut write: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    let mut attempt = 1;
    loop {
        match write().await {
            Ok(value) => return Ok(value),
            Err(err)
                if attempt < MAX_WRITE_ATTEMPTS && crate::commands::analysis::is_busy(&err) =>
            {
                tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}
