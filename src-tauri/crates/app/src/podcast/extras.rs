//! What is fetched when an episode is opened: its chapters and its transcript.
//!
//! Neither comes with a refresh. A feed names them, and they are asked for when
//! someone opens the episode (chapters) or the panel (a transcript), through
//! [`Fetcher`] like anything a feed names, then kept with the row.
//!
//! **What is kept is read again every time**, by the same readers as the first time
//! ([`chapters::read_cache`], [`transcript::read_cache`]): a row may have come from
//! an archive somebody made. Empty, or not what it should be, means to be found
//! again. An archive blanks both, so the first opening after a restore finds them
//! again too.
//!
//! **What is kept needs nobody**, so the switch off and offline mode only stop what
//! would be a request. Local sources stay: a chapter list in the feed's own row, or
//! in the tag of a file on disk, reads the same with either on. Such an answer is not
//! kept, since a better source was not asked.
//!
//! A failure is a word from `FetchFailure::code` and nothing else, like any other
//! failure here. No address reaches the log or the webview.

use std::path::Path;

use chrono::Utc;
use serde::Serialize;
use url::Url;
use waveflow_core::podcast::address::parse_address;
use waveflow_core::podcast::chapters::{self, Chapter, ChapterSource};
use waveflow_core::podcast::id3;
use waveflow_core::podcast::transcript::{self, TranscriptKind, TranscriptLine};
use waveflow_core::podcast::{FetchFailure, MAX_CHAPTERS_BYTES, MAX_TRANSCRIPT_BYTES};

use super::fetch::{FetchError, Fetcher, Origin};
use super::playback::downloaded_file;
use super::refresh::{with_busy_retry, Leases, PodcastError};
use super::store::{self, ChapterSources};

/// How long an answer of "nothing here" holds before it is asked again. Publishers
/// add chapters after publishing, and the enclosure is not worth reading at every
/// opening of a page that has none.
const RECHECK_AFTER_MS: i64 = 24 * 60 * 60 * 1000;

/// How many of an episode's transcripts are tried before giving up. A feed may name
/// sixteen, and each is a request nobody asked for by name.
const MAX_TRANSCRIPT_ATTEMPTS: usize = 4;

/// The words for what was not asked, as a refused command says them.
const OFFLINE: &str = "offline";
const DISABLED: &str = "disabled";

/// An episode's chapters, and why there may be none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EpisodeChapters {
    pub chapters: Vec<Chapter>,
    pub source: Option<ChapterSource>,
    /// Set only when there are no chapters and something could not be asked or
    /// answered. `None` with no chapters is an episode that has none.
    pub failure: Option<String>,
}

/// An episode's transcript. `available` is false when its feed names none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EpisodeTranscript {
    pub available: bool,
    /// Its lines carry times, so the panel can follow the position.
    pub timed: bool,
    pub lines: Vec<TranscriptLine>,
}

impl EpisodeTranscript {
    fn none() -> Self {
        Self {
            available: false,
            timed: false,
            lines: Vec::new(),
        }
    }
}

/// A failure that says nothing about the file: the next opening asks again. Anything
/// else (a 404, a refused address, a file that is not one) is an answer, and holds
/// for [`RECHECK_AFTER_MS`].
fn is_passing(failure: FetchFailure) -> bool {
    match failure {
        FetchFailure::Unreached => true,
        FetchFailure::Status(status) => {
            status == 408 || status == 425 || status == 429 || status >= 500
        }
        _ => false,
    }
}

/// What may be reached from this channel. Decided from the address it was typed
/// with, and with none to go by, the stricter of the two rules.
fn origin_of(feed_url: Option<&str>) -> Origin {
    feed_url
        .and_then(parse_address)
        .map_or_else(Origin::public, |typed| Origin::of(&typed))
}

/// One answer from a request: what it found, whether it holds, and its word.
struct Asked<T> {
    found: Option<T>,
    settled: bool,
    failure: Option<String>,
}

impl<T> Asked<T> {
    fn found(found: T) -> Self {
        Self {
            found: Some(found),
            settled: true,
            failure: None,
        }
    }

    fn failed(err: FetchError) -> Self {
        match err {
            FetchError::Offline => Self {
                found: None,
                settled: false,
                failure: Some(OFFLINE.to_owned()),
            },
            FetchError::Failed(failure) => Self {
                found: None,
                settled: !is_passing(failure),
                failure: Some(failure.code()),
            },
        }
    }
}

async fn ask_chapter_file(fetcher: &Fetcher, raw: &str, origin: Origin) -> Asked<Vec<Chapter>> {
    let Some(url) = parse_address(raw) else {
        return Asked::failed(FetchFailure::AddressRefused.into());
    };
    match fetcher.small_file(&url, origin, MAX_CHAPTERS_BYTES).await {
        Ok(bytes) => match chapters::read_json(&bytes) {
            Some(list) => Asked::found(list),
            None => Asked::failed(FetchFailure::InvalidDocument.into()),
        },
        Err(err) => Asked::failed(err),
    }
}

/// The tag at the start of a stream. A body that holds no tag is an answer like any
/// other: most MP3s have none of these.
async fn ask_stream_tag(fetcher: &Fetcher, url: &Url, origin: Origin) -> Asked<Vec<Chapter>> {
    match fetcher.head_of(url, origin, MAX_CHAPTERS_BYTES).await {
        Ok(bytes) => Asked::found(id3::read_chapters(
            &mut std::io::Cursor::new(bytes),
            MAX_CHAPTERS_BYTES,
        )),
        Err(err) => Asked::failed(err),
    }
}

/// The tag of a file on disk, on the blocking pool. A file that will not open has
/// no chapters: it is not worth a word, and the sweep deals with a row that lost it.
async fn read_file_tag(path: std::path::PathBuf) -> Vec<Chapter> {
    tokio::task::spawn_blocking(move || {
        std::fs::File::open(path).map_or_else(
            |_| Vec::new(),
            |file| id3::read_chapters(&mut std::io::BufReader::new(file), MAX_CHAPTERS_BYTES),
        )
    })
    .await
    .unwrap_or_default()
}

/// An episode's chapters, the first source with a valid one winning: the feed's
/// chapter file, its inline list, the tag of the MP3. `again` is someone pressing
/// Retry, which an answer of nothing does not hold against.
pub(crate) async fn chapters<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    downloads: &Path,
    episode_id: i64,
    again: bool,
) -> Result<EpisodeChapters, PodcastError> {
    let (row, enabled) = {
        let pool = leases.lease().await?;
        let row = store::chapter_sources(&pool, episode_id)
            .await?
            .ok_or(PodcastError::EpisodeGone)?;
        (row, store::is_enabled(&pool).await?)
    };
    let cached = row.chapters_cache.as_deref().and_then(chapters::read_cache);
    // Read against the row and not a clock, so a cache that says nothing readable is
    // no answer, however recently it was written.
    let unreadable = row.chapters_cache.is_some() && cached.is_none();
    let recent = !again
        && !unreadable
        && row
            .chapters_checked_at
            .is_some_and(|at| Utc::now().timestamp_millis() - at < RECHECK_AFTER_MS);

    if let Some((source, list)) = &cached {
        // The chapter file outranks everything else, and only a change in the feed
        // undoes what it said. Below it, the file gets asked again now and then.
        if *source == ChapterSource::Json || row.chapters_url.is_none() || recent {
            return Ok(EpisodeChapters {
                chapters: list.clone(),
                source: Some(*source),
                failure: None,
            });
        }
    }

    let may_ask = enabled && !fetcher.is_offline();
    let refusal = || {
        if enabled {
            OFFLINE.to_owned()
        } else {
            DISABLED.to_owned()
        }
    };
    let origin = origin_of(row.feed_url.as_deref());
    let mut settled = true;
    let mut failure = None;

    let mut from_file = None;
    if let Some(raw) = row.chapters_url.as_deref() {
        // An answer that holds comes first: it needs nobody.
        if recent {
        } else if !may_ask {
            settled = false;
            failure = Some(refusal());
        } else {
            let asked = ask_chapter_file(fetcher, raw, origin).await;
            settled = asked.settled;
            failure = asked.failure;
            from_file = asked.found;
        }
    }

    let inline = row
        .inline_chapters
        .as_deref()
        .map(chapters::read_stored)
        .unwrap_or_default();

    let mut from_tag = Vec::new();
    if from_file.as_ref().map_or(true, Vec::is_empty) && inline.is_empty() {
        let on_disk = row
            .download_file
            .as_deref()
            .and_then(|name| downloaded_file(downloads, episode_id, name));
        let enclosure = row.enclosure_url.as_deref().and_then(parse_address);
        if let Some(path) = on_disk {
            from_tag = read_file_tag(path).await;
        } else if let Some((ChapterSource::Id3, list)) = &cached {
            from_tag.clone_from(list);
        } else if let Some(url) =
            enclosure.filter(|url| id3::may_carry_tag(row.enclosure_type.as_deref(), url))
        {
            if recent {
            } else if !may_ask {
                settled = false;
                failure = failure.or_else(|| Some(refusal()));
            } else {
                let asked = ask_stream_tag(fetcher, &url, origin).await;
                settled = settled && asked.settled;
                failure = failure.or(asked.failure);
                from_tag = asked.found.unwrap_or_default();
            }
        }
    }

    let chosen = chapters::choose(from_file, inline, from_tag);
    if settled && !recent {
        keep_chapters(leases, episode_id, &row, chosen.as_ref()).await;
    }
    Ok(match chosen {
        Some((source, list)) => EpisodeChapters {
            chapters: list,
            source: Some(source),
            failure: None,
        },
        None => EpisodeChapters {
            chapters: Vec::new(),
            source: None,
            failure,
        },
    })
}

/// A write nobody waits for: losing it costs asking again next time.
async fn keep_chapters<L: Leases>(
    leases: &L,
    episode_id: i64,
    row: &ChapterSources,
    chosen: Option<&(ChapterSource, Vec<Chapter>)>,
) {
    let cache = chosen.and_then(|(source, list)| chapters::write_cache(*source, list));
    let Ok(pool) = leases.lease().await else {
        return;
    };
    if let Err(err) =
        with_busy_retry(|| store::record_chapters(&pool, episode_id, row, cache.as_deref())).await
    {
        tracing::warn!(%err, "podcast: chapters not kept");
    }
}

fn transcript_answer(kind: TranscriptKind, lines: Vec<TranscriptLine>) -> EpisodeTranscript {
    let timed = kind.is_timed() && lines.iter().all(|line| line.start_ms.is_some());
    EpisodeTranscript {
        available: true,
        timed,
        lines,
    }
}

/// An episode's transcript, for a panel someone opened. The best kind the feed names
/// that holds up: a host may serve an error page as its WebVTT while its text is
/// fine. With nothing kept, a refusal with the switch off or in offline mode.
pub(crate) async fn transcript<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    episode_id: i64,
) -> Result<EpisodeTranscript, PodcastError> {
    let (row, enabled) = {
        let pool = leases.lease().await?;
        let row = store::transcript_sources(&pool, episode_id)
            .await?
            .ok_or(PodcastError::EpisodeGone)?;
        (row, store::is_enabled(&pool).await?)
    };

    if let (Some(kind), Some(stored)) = (
        row.transcript_kind
            .as_deref()
            .and_then(TranscriptKind::from_stored),
        row.transcript_cache.as_deref(),
    ) {
        if let Ok(document) = transcript::read_cache(kind, stored) {
            let lines = transcript::lines_of(&document);
            if !lines.is_empty() {
                return Ok(transcript_answer(kind, lines));
            }
        }
    }

    let Some(stored_refs) = row.transcripts else {
        return Ok(EpisodeTranscript::none());
    };
    let refs = transcript::read_stored_refs(&stored_refs);
    if refs.is_empty() {
        return Ok(EpisodeTranscript::none());
    }
    if !enabled {
        return Err(PodcastError::Disabled);
    }
    if fetcher.is_offline() {
        return Err(PodcastError::Offline);
    }

    let origin = origin_of(row.feed_url.as_deref());
    let mut first_failure = None;
    for candidate in transcript::ranked(&refs)
        .into_iter()
        .take(MAX_TRANSCRIPT_ATTEMPTS)
    {
        let body = match parse_address(&candidate.url) {
            Some(url) => fetcher.small_file(&url, origin, MAX_TRANSCRIPT_BYTES).await,
            None => Err(FetchFailure::AddressRefused.into()),
        };
        let bytes = match body {
            Ok(bytes) => bytes,
            Err(FetchError::Offline) => return Err(PodcastError::Offline),
            Err(FetchError::Failed(failure)) => {
                first_failure.get_or_insert(failure);
                continue;
            }
        };
        let lines = transcript::read(candidate.kind, &bytes)
            .map(|document| transcript::lines_of(&document))
            .unwrap_or_default();
        if lines.is_empty() {
            first_failure.get_or_insert(FetchFailure::InvalidDocument);
            continue;
        }
        let text = transcript::decode(&bytes);
        if let Ok(pool) = leases.lease().await {
            if let Err(err) = with_busy_retry(|| {
                store::record_transcript(
                    &pool,
                    episode_id,
                    &stored_refs,
                    candidate.kind.stored(),
                    &text,
                )
            })
            .await
            {
                tracing::warn!(%err, "podcast: transcript not kept");
            }
        }
        return Ok(transcript_answer(candidate.kind, lines));
    }
    Err(PodcastError::Failed(
        first_failure.unwrap_or(FetchFailure::InvalidDocument),
    ))
}
