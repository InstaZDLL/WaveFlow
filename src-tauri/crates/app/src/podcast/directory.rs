//! The podcast directory: searching it, reading a country's chart, and finding
//! the feed of a show someone picked there.
//!
//! What its answers mean is [`waveflow_core::podcast::directory`]. What lives
//! here is the asking, under the rules every podcast request keeps:
//!
//! - **The switch and offline mode** are asked before every request, a picture's
//!   included: with podcasts off nothing here contacts anybody, and a cached
//!   answer is not served either, so the page cannot look as if it still were.
//! - **Every request goes through [`Fetcher`]**, held to public addresses
//!   whatever it redirects to, bounded in size and time.
//! - **Nothing asked is logged**: not a term, not a country, not an address. A
//!   failure is logged as a word, as a feed's is.
//! - **The directory is asked at most [`MAX_PER_WINDOW`] times a minute** for
//!   the whole app ([`Directory::paced`]): it refuses a client past about twenty,
//!   per address. A request past the budget waits for a slot rather than
//!   failing. Pictures come from its image host, which has no such budget, so
//!   they only wait for one of [`ARTWORK_AT_ONCE`] slots.
//! - **Answers are kept in memory**, a chart for [`CHART_TTL`] and a search for
//!   [`SEARCH_TTL`], so browsing back and forth asks nothing again. Nothing is
//!   written to disk: once the app quits, nothing remains of what was browsed.
//! - **A feed found here is held to public addresses**, as one out of an OPML
//!   list is: the directory is a third party, and an address it hands out must
//!   not make the app reach into the user's network.
//! - **No feed address goes to the webview.** A show is asked for by its id in
//!   the directory, and [`Directory::feed_of`] finds its feed here.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use url::Url;
use waveflow_core::podcast::address::parse_address;
use waveflow_core::podcast::directory::{
    chart_url, lookup_url, parse_chart, parse_search, search_url, DirectoryError, DirectoryShow,
    MAX_DIRECTORY_BYTES,
};
use waveflow_core::podcast::FetchFailure;

use super::fetch::{FetchError, Fetcher, Origin};
use super::refresh::{enabled_or_refuse, Leases, PodcastError};

/// How many requests the directory gets in any [`WINDOW`]. It is documented as
/// about twenty a minute and has been seen refusing below that, so this keeps
/// a margin.
pub(crate) const MAX_PER_WINDOW: usize = 15;

/// The window [`MAX_PER_WINDOW`] is counted over.
pub(crate) const WINDOW: Duration = Duration::from_secs(60);

/// How long a chart is kept. The directory updates them about once a day.
pub(crate) const CHART_TTL: Duration = Duration::from_secs(6 * 3_600);

/// How long a search is kept: long enough to go back to it, short enough that
/// a show published since turns up.
pub(crate) const SEARCH_TTL: Duration = Duration::from_secs(10 * 60);

/// How many answers are kept. Past this, the oldest goes.
const MAX_ANSWERS: usize = 64;

/// How many shows' feed and picture addresses are kept. Past this the record
/// starts over, and a show picked from a list no longer kept is looked up.
const MAX_SHOWS_KNOWN: usize = 10_000;

/// The largest picture read, in bytes. A 300-pixel cover weighs about 40 KB.
const MAX_ARTWORK_BYTES: usize = 1024 * 1024;

/// How many bytes of pictures are kept in memory. Past this, the oldest go.
const ARTWORK_BUDGET: usize = 32 * 1024 * 1024;

/// How many shows' pictures are kept, a show without one included: a miss
/// weighs no bytes, so the budget alone would never let one go.
const MAX_PICTURES_KEPT: usize = 4_000;

/// How many pictures are asked for at once.
pub(crate) const ARTWORK_AT_ONCE: usize = 6;

/// How a request reaches the directory. [`Fetcher`] in the app; a test answers
/// from a table.
pub(crate) trait Asker: Sync {
    fn ask(
        &self,
        url: &Url,
        max_bytes: usize,
    ) -> impl Future<Output = Result<Vec<u8>, FetchError>> + Send;
}

impl Asker for Fetcher {
    async fn ask(&self, url: &Url, max_bytes: usize) -> Result<Vec<u8>, FetchError> {
        self.small_file(url, Origin::public(), max_bytes).await
    }
}

/// What is asked of the directory, as it is kept.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Question {
    Chart { country: String, genre: Option<u32> },
    Search { country: String, term: String },
}

impl Question {
    fn ttl(&self) -> Duration {
        match self {
            Self::Chart { .. } => CHART_TTL,
            Self::Search { .. } => SEARCH_TTL,
        }
    }
}

struct Answer {
    at: Instant,
    shows: Arc<Vec<DirectoryShow>>,
}

/// What is known of a show beyond what the webview gets.
#[derive(Debug, Clone, Default)]
struct Known {
    feed_url: Option<String>,
    artwork_url: Option<String>,
}

/// Where shows being recorded come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorded {
    /// An answer just received: what it says replaces what was known.
    Fresh,
    /// An answer kept in memory: it fills only what is not known.
    IfUnknown,
}

/// Pictures as `data:` addresses, oldest first out.
#[derive(Default)]
struct Pictures {
    by_id: HashMap<u64, Option<Arc<str>>>,
    order: VecDeque<u64>,
    bytes: usize,
}

impl Pictures {
    fn put(&mut self, id: u64, picture: Option<Arc<str>>) {
        if self.by_id.contains_key(&id) {
            return;
        }
        self.bytes += picture.as_ref().map_or(0, |picture| picture.len());
        self.by_id.insert(id, picture);
        self.order.push_back(id);
        while self.bytes > ARTWORK_BUDGET || self.order.len() > MAX_PICTURES_KEPT {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(Some(picture)) = self.by_id.remove(&oldest) {
                self.bytes -= picture.len();
            }
        }
    }
}

/// The directory, as the whole app asks it.
pub(crate) struct Directory {
    sent: Mutex<VecDeque<Instant>>,
    answers: Mutex<HashMap<Question, Answer>>,
    known: Mutex<HashMap<u64, Known>>,
    pictures: Mutex<Pictures>,
    picture_slots: Semaphore,
}

impl Default for Directory {
    fn default() -> Self {
        Self {
            sent: Mutex::new(VecDeque::new()),
            answers: Mutex::new(HashMap::new()),
            known: Mutex::new(HashMap::new()),
            pictures: Mutex::new(Pictures::default()),
            picture_slots: Semaphore::new(ARTWORK_AT_ONCE),
        }
    }
}

/// The one the app uses: the directory's budget is per address, so per
/// process, whichever profile asks.
pub(crate) fn shared() -> &'static Directory {
    static DIRECTORY: OnceLock<Directory> = OnceLock::new();
    DIRECTORY.get_or_init(Directory::default)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What a failed request is told as.
fn refused(err: FetchError) -> PodcastError {
    match err {
        // The directory says no to a client past its budget with a 403.
        FetchError::Failed(FetchFailure::Status(403 | 429)) => PodcastError::RateLimited,
        other => other.into(),
    }
}

fn unreadable(err: DirectoryError) -> PodcastError {
    match err {
        DirectoryError::TooLarge => PodcastError::Failed(FetchFailure::TooLarge),
        DirectoryError::Invalid => PodcastError::DirectoryInvalid,
    }
}

/// Logs a failure as its word, and nothing of what was asked.
fn log_failure(what: &'static str, err: &PodcastError) {
    let word = match err {
        PodcastError::Disabled | PodcastError::Offline => return,
        PodcastError::RateLimited => "rate_limited".to_owned(),
        PodcastError::DirectoryInvalid => "directory_invalid".to_owned(),
        PodcastError::Failed(failure) => failure.code(),
        _ => "other".to_owned(),
    };
    tracing::debug!(what, code = %word, "podcast directory request failed");
}

impl Directory {
    /// Waits until the directory may be asked again, then counts the request.
    pub(crate) async fn paced(&self) {
        loop {
            let wait = {
                let mut sent = lock(&self.sent);
                let now = Instant::now();
                while sent
                    .front()
                    .is_some_and(|at| now.duration_since(*at) >= WINDOW)
                {
                    sent.pop_front();
                }
                if sent.len() < MAX_PER_WINDOW {
                    sent.push_back(now);
                    return;
                }
                let oldest = *sent.front().expect("a full window has a first request");
                WINDOW.saturating_sub(now.duration_since(oldest))
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Asks the directory, once the switch, offline mode and the budget allow.
    async fn ask_directory<L: Leases, A: Asker>(
        &self,
        leases: &L,
        asker: &A,
        url: &Url,
    ) -> Result<Vec<u8>, PodcastError> {
        enabled_or_refuse(leases).await?;
        self.paced().await;
        // The wait may have been long: the switch may have gone off meanwhile.
        enabled_or_refuse(leases).await?;
        asker.ask(url, MAX_DIRECTORY_BYTES).await.map_err(refused)
    }

    /// A kept answer still fresh. Its shows are recorded again, since the record
    /// may have started over since it was kept, but only where nothing is known:
    /// a newer answer may have named another feed or picture since.
    fn cached(&self, question: &Question) -> Option<Arc<Vec<DirectoryShow>>> {
        let shows = {
            let answers = lock(&self.answers);
            let answer = answers.get(question)?;
            (answer.at.elapsed() < question.ttl()).then(|| Arc::clone(&answer.shows))?
        };
        self.remember(&shows, Recorded::IfUnknown);
        Some(shows)
    }

    fn keep(&self, question: Question, shows: Vec<DirectoryShow>) -> Arc<Vec<DirectoryShow>> {
        self.remember(&shows, Recorded::Fresh);
        let shows = Arc::new(shows);
        let mut answers = lock(&self.answers);
        answers.retain(|kept, answer| answer.at.elapsed() < kept.ttl());
        while answers.len() >= MAX_ANSWERS {
            let Some(oldest) = answers
                .iter()
                .min_by_key(|(_, answer)| answer.at)
                .map(|(question, _)| question.clone())
            else {
                break;
            };
            answers.remove(&oldest);
        }
        answers.insert(
            question,
            Answer {
                at: Instant::now(),
                shows: Arc::clone(&shows),
            },
        );
        shows
    }

    /// Records each show's feed and picture, a feed already known kept when an
    /// answer has none. An answer read again from memory only fills what is not
    /// known, so it never replaces what a newer one said.
    fn remember(&self, shows: &[DirectoryShow], recorded: Recorded) {
        let mut known = lock(&self.known);
        // A full record starts over for an answer just received. An answer read
        // again from memory never empties it, or it would put back older feeds in
        // place of the newer ones it emptied; the record stays bounded all the
        // same, by the answers kept ([`MAX_ANSWERS`] of at most 200 shows each).
        if recorded == Recorded::Fresh && known.len() + shows.len() > MAX_SHOWS_KNOWN {
            known.clear();
        }
        let fill = |slot: &mut Option<String>, value: &Option<String>| {
            if value.is_some() && (recorded == Recorded::Fresh || slot.is_none()) {
                slot.clone_from(value);
            }
        };
        for show in shows {
            let entry = known.entry(show.id).or_default();
            fill(&mut entry.feed_url, &show.feed_url);
            fill(&mut entry.artwork_url, &show.artwork_url);
        }
    }

    /// What the record starting over does, for a test.
    #[cfg(test)]
    pub(crate) fn forget_shows(&self) {
        lock(&self.known).clear();
    }

    /// Every kept answer as if it had been kept `by` earlier, for a test. A
    /// paused clock cannot do it with a database in the test: the runtime
    /// moves it on to the pool's timers while a query runs.
    #[cfg(test)]
    pub(crate) fn age_answers(&self, by: Duration) {
        for answer in lock(&self.answers).values_mut() {
            answer.at = answer.at.checked_sub(by).expect("a clock far enough along");
        }
    }

    fn known_feed(&self, id: u64) -> Option<String> {
        lock(&self.known)
            .get(&id)
            .and_then(|known| known.feed_url.clone())
    }

    /// `country`'s chart, narrowed to `genre`. Its shows' feeds are looked up
    /// together right after, in one more request, so the page can tell which
    /// are already followed; that lookup failing leaves the chart as it is.
    pub(crate) async fn chart<L: Leases, A: Asker>(
        &self,
        leases: &L,
        asker: &A,
        country: &str,
        genre: Option<u32>,
    ) -> Result<Arc<Vec<DirectoryShow>>, PodcastError> {
        let question = Question::Chart {
            country: country.to_owned(),
            genre,
        };
        enabled_or_refuse(leases).await?;
        if let Some(shows) = self.cached(&question) {
            return Ok(shows);
        }
        let result = async {
            let bytes = self
                .ask_directory(leases, asker, &chart_url(country, genre))
                .await?;
            let mut shows = parse_chart(&bytes).map_err(unreadable)?;
            if !shows.is_empty() {
                let ids: Vec<u64> = shows.iter().map(|show| show.id).collect();
                match self
                    .ask_directory(leases, asker, &lookup_url(&ids, country))
                    .await
                    .and_then(|bytes| parse_search(&bytes).map_err(unreadable))
                {
                    Ok(found) => {
                        let found: HashMap<u64, DirectoryShow> =
                            found.into_iter().map(|show| (show.id, show)).collect();
                        for show in &mut shows {
                            if let Some(looked_up) = found.get(&show.id) {
                                show.feed_url.clone_from(&looked_up.feed_url);
                                // A chart says neither; the lookup does, as a search does.
                                show.episode_count = looked_up.episode_count;
                                show.explicit = looked_up.explicit;
                                // A chart names a sub-genre; a lookup names its parent too.
                                if show.genre_id.is_none() {
                                    show.genre_id = looked_up.genre_id;
                                }
                            }
                        }
                    }
                    // What stops the whole request stops the chart too; only a
                    // failure of the lookup itself leaves the shows without feeds.
                    Err(
                        err @ (PodcastError::Disabled
                        | PodcastError::Offline
                        | PodcastError::ProfileChanged),
                    ) => return Err(err),
                    Err(err) => log_failure("lookup", &err),
                }
            }
            // Every show of a narrowed chart is of its genre, whatever sub-genre it
            // names and whichever genre its lookup puts first.
            if let Some(genre) = genre {
                for show in &mut shows {
                    show.genre_id = Some(genre);
                }
            }
            Ok(shows)
        }
        .await;
        match result {
            Ok(shows) => Ok(self.keep(question, shows)),
            Err(err) => {
                log_failure("chart", &err);
                Err(err)
            }
        }
    }

    /// The shows a search for `term` finds in `country`'s storefront.
    pub(crate) async fn search<L: Leases, A: Asker>(
        &self,
        leases: &L,
        asker: &A,
        country: &str,
        term: &str,
    ) -> Result<Arc<Vec<DirectoryShow>>, PodcastError> {
        let question = Question::Search {
            country: country.to_owned(),
            term: term.to_owned(),
        };
        enabled_or_refuse(leases).await?;
        if let Some(shows) = self.cached(&question) {
            return Ok(shows);
        }
        let result = async {
            let bytes = self
                .ask_directory(leases, asker, &search_url(term, country))
                .await?;
            parse_search(&bytes).map_err(unreadable)
        }
        .await;
        match result {
            Ok(shows) => Ok(self.keep(question, shows)),
            Err(err) => {
                log_failure("search", &err);
                Err(err)
            }
        }
    }

    /// The feed of the show `id`: the one an answer gave, or else a lookup's.
    /// Refused unless it is an address on the public internet.
    pub(crate) async fn feed_of<L: Leases, A: Asker>(
        &self,
        leases: &L,
        asker: &A,
        country: &str,
        id: u64,
    ) -> Result<Url, PodcastError> {
        enabled_or_refuse(leases).await?;
        let raw = match self.known_feed(id) {
            Some(raw) => raw,
            None => {
                let found = self
                    .ask_directory(leases, asker, &lookup_url(&[id], country))
                    .await
                    .and_then(|bytes| parse_search(&bytes).map_err(unreadable))
                    .inspect_err(|err| log_failure("lookup", err))?;
                self.remember(&found, Recorded::Fresh);
                found
                    .into_iter()
                    .find(|show| show.id == id)
                    .and_then(|show| show.feed_url)
                    .ok_or(PodcastError::NoFeed)?
            }
        };
        let url = parse_address(&raw).ok_or(PodcastError::Failed(FetchFailure::AddressRefused))?;
        if !Origin::of(&url).is_public() {
            return Err(PodcastError::Failed(FetchFailure::AddressRefused));
        }
        Ok(url)
    }

    /// The picture of the show `id` as a `data:` address, `None` when it has
    /// none or what came back is not a usable image. Kept in memory, a miss
    /// included, so a list scrolled back and forth asks nothing again.
    pub(crate) async fn picture<L: Leases, A: Asker>(
        &self,
        leases: &L,
        asker: &A,
        id: u64,
    ) -> Result<Option<Arc<str>>, PodcastError> {
        enabled_or_refuse(leases).await?;
        if let Some(picture) = lock(&self.pictures).by_id.get(&id) {
            return Ok(picture.clone());
        }
        let Some(raw) = lock(&self.known)
            .get(&id)
            .and_then(|known| known.artwork_url.clone())
        else {
            return Ok(None);
        };
        let Some(url) = parse_address(&raw) else {
            return Ok(None);
        };
        let _slot = self
            .picture_slots
            .acquire()
            .await
            .map_err(|_| PodcastError::Failed(FetchFailure::Unreached))?;
        // The wait may have been long: the switch may have gone off meanwhile, and
        // a picture is served only while it is on, a kept one included.
        enabled_or_refuse(leases).await?;
        // Another call for the same show may have filled it while this one waited.
        if let Some(picture) = lock(&self.pictures).by_id.get(&id) {
            return Ok(picture.clone());
        }
        let picture = match asker.ask(&url, MAX_ARTWORK_BYTES).await {
            Ok(bytes) => tokio::task::spawn_blocking(move || data_url(&bytes))
                .await
                .ok()
                .flatten()
                .map(Arc::from),
            Err(FetchError::Offline) => return Err(PodcastError::Offline),
            Err(FetchError::Failed(failure)) => {
                tracing::debug!(code = %failure.code(), "podcast directory picture not fetched");
                // Only what says something about the picture is kept: a host that
                // is down, busy or refusing a client past its budget may answer later.
                if !matches!(
                    failure,
                    FetchFailure::Status(404 | 410)
                        | FetchFailure::TooLarge
                        | FetchFailure::AddressRefused
                        | FetchFailure::TooManyRedirects
                ) {
                    return Ok(None);
                }
                None
            }
        };
        lock(&self.pictures).put(id, picture.clone());
        Ok(picture)
    }
}

/// `bytes` as a `data:` address, if they are a cover the app accepts: JPEG,
/// PNG, WebP or GIF, of a sane size, read from the header alone.
pub(crate) fn data_url(bytes: &[u8]) -> Option<String> {
    if !waveflow_core::podcast::accepts_cover(bytes) {
        return None;
    }
    let kind = if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else {
        return None;
    };
    Some(format!(
        "data:{kind};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}
