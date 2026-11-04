//! The `podcast_channel` and `podcast_episode` tables.
//!
//! No read here hands an address to the webview: a row says whether it has one,
//! and whatever opens it (a refresh, playback) asks for it by id. A screenshot
//! of the page, or a log of an IPC payload, then has no token to carry.

use chrono::Utc;
use serde::Serialize;
use sqlx::{FromRow, SqliteConnection, SqlitePool};
use url::Url;
use waveflow_core::podcast::address::{item_key, parse_address};
use waveflow_core::podcast::{Feed, FeedEpisode};

use super::fetch::Validators;

/// The setting that turns all of this on, off by default. Mirrored in
/// `settingsRegistry.ts`, where the frontend reads and resets it.
pub(crate) const ENABLED_KEY: &str = "podcasts.enabled";

/// When this profile's channels were last all asked, in epoch milliseconds. The
/// six hours count from here, so a session shorter than that still gets its
/// check. Not a preference, so not in `settingsRegistry.ts`.
const LAST_CHECK_KEY: &str = "podcasts.last_check_at";

/// The most this profile keeps downloaded, in GiB. Mirrored in
/// `settingsRegistry.ts`, where the frontend reads and resets it.
pub(crate) const DOWNLOAD_QUOTA_KEY: &str = "podcasts.download_quota_gb";

/// The quota nobody has changed.
pub(crate) const DEFAULT_DOWNLOAD_QUOTA_GB: u64 = 10;

const GIB: u64 = 1024 * 1024 * 1024;

/// How many rows one channel keeps once they have left its feed. A feed may
/// hand out 500 fresh guids at every refresh, and without a ceiling that is a
/// table growing without end. Only rows nobody has touched are removed.
const MAX_STORED_EPISODES: i64 = 2_000;

/// A channel, as the page shows it. Holds no address.
#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
pub struct PodcastChannel {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub language: Option<String>,
    /// The feed names a website. Opening it is a command's to do, by id: a private
    /// feed's link may hold its token like any other address.
    pub has_site_url: bool,
    pub artwork_hash: Option<String>,
    /// The file `artwork_hash` names, when it is still on disk. Filled in by the
    /// command, which knows where the cache is.
    #[sqlx(skip)]
    pub artwork_path: Option<String>,
    /// False in a profile restored from an archive, which carries no address.
    /// Nothing refreshes until the feed is subscribed to again, which finds
    /// this row and keeps every position.
    pub has_feed_address: bool,
    pub episode_count: i64,
    pub unplayed_count: i64,
    pub last_refreshed_at: Option<i64>,
    /// One word from `FetchFailure::code`, `None` after a success.
    pub last_error: Option<String>,
    /// The speed this channel's episodes start at; `None` is the normal one.
    pub playback_speed: Option<f64>,
    pub created_at: i64,
}

const CHANNEL_SELECT: &str = "
    SELECT c.id, c.title, c.description, c.author, c.language,
           c.site_url IS NOT NULL AS has_site_url,
           c.artwork_hash,
           c.feed_url IS NOT NULL AS has_feed_address,
           (SELECT COUNT(*) FROM podcast_episode e
             WHERE e.channel_id = c.id AND e.deleted_at IS NULL) AS episode_count,
           (SELECT COUNT(*) FROM podcast_episode e
             WHERE e.channel_id = c.id AND e.deleted_at IS NULL
               AND e.played_at IS NULL) AS unplayed_count,
           c.last_refreshed_at, c.last_error, c.playback_speed, c.created_at
      FROM podcast_channel c";

/// An episode, as the page shows it. Holds no address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, FromRow)]
pub struct PodcastEpisode {
    pub id: i64,
    pub channel_id: i64,
    pub title: String,
    pub description: Option<String>,
    pub published_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub season: Option<i64>,
    pub episode_number: Option<i64>,
    pub enclosure_type: Option<String>,
    pub enclosure_length: Option<i64>,
    /// There is an address to stream from. False in a restored profile until
    /// the feed is subscribed to again.
    pub has_enclosure: bool,
    pub position_ms: i64,
    pub played_at: Option<i64>,
    pub last_opened_at: Option<i64>,
    pub downloaded: bool,
    pub download_size: Option<i64>,
    pub has_chapters: bool,
    pub has_transcript: bool,
}

const EPISODE_SELECT: &str = "
    SELECT id, channel_id, title, description, published_at, duration_ms, season,
           episode_number, enclosure_type, enclosure_length,
           enclosure_url IS NOT NULL AS has_enclosure,
           position_ms, played_at, last_opened_at,
           download_file IS NOT NULL AS downloaded,
           download_size,
           chapters_url IS NOT NULL OR inline_chapters IS NOT NULL
               OR chapters_cache IS NOT NULL AS has_chapters,
           transcripts IS NOT NULL OR transcript_cache IS NOT NULL AS has_transcript
      FROM podcast_episode";

/// What a refresh needs of a channel.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub(crate) struct ChannelSource {
    pub id: i64,
    pub feed_url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub image_url: Option<String>,
    pub artwork_hash: Option<String>,
}

pub(crate) async fn is_enabled(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(ENABLED_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(matches!(value.as_deref(), Some("true" | "1")))
}

pub(crate) async fn last_check_at(pool: &SqlitePool) -> Result<Option<i64>, sqlx::Error> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(LAST_CHECK_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(value.and_then(|value| value.parse().ok()))
}

pub(crate) async fn stamp_last_check(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'int', ?)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            value_type = excluded.value_type,
            updated_at = excluded.updated_at",
    )
    .bind(LAST_CHECK_KEY)
    .bind(now.to_string())
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn list_channels(pool: &SqlitePool) -> Result<Vec<PodcastChannel>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{CHANNEL_SELECT} ORDER BY c.title COLLATE NOCASE, c.id"
    )))
    .fetch_all(pool)
    .await
}

pub(crate) async fn channel(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<PodcastChannel>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{CHANNEL_SELECT} WHERE c.id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// A channel's episodes that the user has not removed, newest first and undated
/// ones last.
pub(crate) async fn list_episodes(
    pool: &SqlitePool,
    channel_id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<PodcastEpisode>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{EPISODE_SELECT}
          WHERE channel_id = ? AND deleted_at IS NULL
          ORDER BY published_at IS NULL, published_at DESC, id DESC
          LIMIT ? OFFSET ?"
    )))
    .bind(channel_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// One episode as the page shows it. One the user removed is gone.
pub(crate) async fn episode(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<PodcastEpisode>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{EPISODE_SELECT} WHERE id = ? AND deleted_at IS NULL"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// A channel by the hash of its address, and whether it still holds one.
pub(crate) async fn find_by_key(
    pool: &SqlitePool,
    feed_key: &str,
) -> Result<Option<(i64, bool)>, sqlx::Error> {
    sqlx::query_as("SELECT id, feed_url IS NOT NULL FROM podcast_channel WHERE feed_key = ?")
        .bind(feed_key)
        .fetch_optional(pool)
        .await
}

/// The channels a refresh can reach, oldest refresh first, so a pass that is
/// stopped short gets further next time rather than repeating the same ones.
pub(crate) async fn refreshable(pool: &SqlitePool) -> Result<Vec<ChannelSource>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, feed_url, etag, last_modified, image_url, artwork_hash
           FROM podcast_channel
          WHERE feed_url IS NOT NULL
          ORDER BY last_refreshed_at IS NOT NULL, last_refreshed_at, id",
    )
    .fetch_all(pool)
    .await
}

pub(crate) async fn source_of(
    pool: &SqlitePool,
    channel_id: i64,
) -> Result<Option<ChannelSource>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, feed_url, etag, last_modified, image_url, artwork_hash
           FROM podcast_channel
          WHERE id = ? AND feed_url IS NOT NULL",
    )
    .bind(channel_id)
    .fetch_optional(pool)
    .await
}

/// `(title, address)` of every channel that has an address, for an OPML export.
pub(crate) async fn export_rows(pool: &SqlitePool) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT title, feed_url FROM podcast_channel
          WHERE feed_url IS NOT NULL
          ORDER BY title COLLATE NOCASE, id",
    )
    .fetch_all(pool)
    .await
}

/// How many channels a restored archive left without an address.
pub(crate) async fn count_without_address(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM podcast_channel WHERE feed_url IS NULL")
        .fetch_one(pool)
        .await
}

/// Stores a channel someone subscribed to, and its episodes, in one transaction,
/// and answers the channel's id.
///
/// `feed_key` finds a channel that exists, including one a restored archive left
/// without an address, which gets it back here. See [`store_episodes`] for how
/// its episodes are matched. Only for subscribing: this creates the row, so a
/// refresh going through it would bring back a channel unsubscribed from while
/// its feed was being read.
pub(crate) async fn apply_feed(
    pool: &SqlitePool,
    feed_key: &str,
    feed_url: &str,
    feed: &Feed,
    validators: &Validators,
) -> Result<i64, sqlx::Error> {
    let now = Utc::now().timestamp_millis();
    let mut tx = pool.begin().await?;
    let channel_id: i64 = sqlx::query_scalar(
        "INSERT INTO podcast_channel
            (feed_key, feed_url, title, description, author, language, site_url, image_url,
             etag, last_modified, last_refreshed_at, last_error, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?)
         ON CONFLICT (feed_key) DO UPDATE SET
            feed_url = excluded.feed_url,
            title = excluded.title,
            description = excluded.description,
            author = excluded.author,
            language = excluded.language,
            site_url = excluded.site_url,
            image_url = excluded.image_url,
            etag = excluded.etag,
            last_modified = excluded.last_modified,
            last_refreshed_at = excluded.last_refreshed_at,
            last_error = NULL,
            updated_at = excluded.updated_at
         RETURNING id",
    )
    .bind(feed_key)
    .bind(feed_url)
    .bind(&feed.title)
    .bind(&feed.description)
    .bind(&feed.author)
    .bind(&feed.language)
    .bind(&feed.site_url)
    .bind(&feed.image_url)
    .bind(&validators.etag)
    .bind(&validators.last_modified)
    .bind(now)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    store_episodes(&mut tx, channel_id, feed, now).await?;
    tx.commit().await?;
    Ok(channel_id)
}

/// Stores what a refresh read into the channel it read it for. False, with nothing
/// written, when that channel is gone: the user unsubscribed while the feed was
/// being read, and that has to stick.
pub(crate) async fn refresh_feed(
    pool: &SqlitePool,
    channel_id: i64,
    feed: &Feed,
    validators: &Validators,
) -> Result<bool, sqlx::Error> {
    let now = Utc::now().timestamp_millis();
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "UPDATE podcast_channel SET
            title = ?,
            description = ?,
            author = ?,
            language = ?,
            site_url = ?,
            image_url = ?,
            etag = ?,
            last_modified = ?,
            last_refreshed_at = ?,
            last_error = NULL,
            updated_at = ?
          WHERE id = ?",
    )
    .bind(&feed.title)
    .bind(&feed.description)
    .bind(&feed.author)
    .bind(&feed.language)
    .bind(&feed.site_url)
    .bind(&feed.image_url)
    .bind(&validators.etag)
    .bind(&validators.last_modified)
    .bind(now)
    .bind(now)
    .bind(channel_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        return Ok(false);
    }
    store_episodes(&mut tx, channel_id, feed, now).await?;
    tx.commit().await?;
    Ok(true)
}

/// A feed's episodes, each matched by its key: a re-published one is updated and
/// keeps its position, and one the user removed stays removed. A row no longer
/// in the feed stays too, since its position is the user's, up to
/// [`MAX_STORED_EPISODES`].
async fn store_episodes(
    conn: &mut SqliteConnection,
    channel_id: i64,
    feed: &Feed,
    now: i64,
) -> Result<(), sqlx::Error> {
    for episode in &feed.episodes {
        upsert_episode(&mut *conn, channel_id, episode, now).await?;
    }
    prune(conn, channel_id).await
}

/// A feed that now names other sources makes what was found through the old ones
/// stale. Rows on the old side of `ON CONFLICT`, the feed's on the `excluded` side.
const SAME_CHAPTER_SOURCES: &str = "chapters_url IS excluded.chapters_url
    AND inline_chapters IS excluded.inline_chapters
    AND enclosure_url IS excluded.enclosure_url";
const SAME_TRANSCRIPTS: &str = "transcripts IS excluded.transcripts";

async fn upsert_episode(
    conn: &mut SqliteConnection,
    channel_id: i64,
    episode: &FeedEpisode,
    now: i64,
) -> Result<(), sqlx::Error> {
    let inline_chapters = (!episode.inline_chapters.is_empty())
        .then(|| serde_json::to_string(&episode.inline_chapters).ok())
        .flatten();
    let transcripts = (!episode.transcripts.is_empty())
        .then(|| serde_json::to_string(&episode.transcripts).ok())
        .flatten();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO podcast_episode
            (channel_id, item_key, enclosure_url, enclosure_type, enclosure_length, title,
             description, published_at, duration_ms, season, episode_number, chapters_url,
             inline_chapters, transcripts, first_seen_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (channel_id, item_key) DO UPDATE SET
            enclosure_url = excluded.enclosure_url,
            enclosure_type = excluded.enclosure_type,
            enclosure_length = excluded.enclosure_length,
            title = excluded.title,
            description = excluded.description,
            published_at = excluded.published_at,
            duration_ms = excluded.duration_ms,
            season = excluded.season,
            episode_number = excluded.episode_number,
            chapters_url = excluded.chapters_url,
            inline_chapters = excluded.inline_chapters,
            transcripts = excluded.transcripts,
            chapters_cache = CASE WHEN {SAME_CHAPTER_SOURCES} THEN chapters_cache END,
            chapters_checked_at = CASE WHEN {SAME_CHAPTER_SOURCES} THEN chapters_checked_at END,
            transcript_cache = CASE WHEN {SAME_TRANSCRIPTS} THEN transcript_cache END,
            transcript_kind = CASE WHEN {SAME_TRANSCRIPTS} THEN transcript_kind END,
            transcript_checked_at = CASE WHEN {SAME_TRANSCRIPTS} THEN transcript_checked_at END,
            updated_at = excluded.updated_at
         WHERE podcast_episode.deleted_at IS NULL"
    )))
    .bind(channel_id)
    .bind(item_key(&episode.guid))
    .bind(&episode.enclosure_url)
    .bind(&episode.enclosure_type)
    .bind(episode.enclosure_length)
    .bind(&episode.title)
    .bind(&episode.description)
    .bind(episode.published_at)
    .bind(episode.duration_ms)
    .bind(episode.season)
    .bind(episode.episode_number)
    .bind(&episode.chapters_url)
    .bind(inline_chapters)
    .bind(transcripts)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The oldest rows past [`MAX_STORED_EPISODES`] that hold nothing of the user's:
/// no position, not played, not downloaded, not removed by hand.
async fn prune(conn: &mut SqliteConnection, channel_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM podcast_episode
          WHERE id IN (
            SELECT id FROM podcast_episode
             WHERE channel_id = ?1
             ORDER BY published_at IS NULL, published_at DESC, id DESC
             LIMIT -1 OFFSET ?2
          )
            AND position_ms = 0
            AND played_at IS NULL
            AND download_file IS NULL
            AND deleted_at IS NULL",
    )
    .bind(channel_id)
    .bind(MAX_STORED_EPISODES)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A channel's feed has moved: it is known by the new address from now on. Refused
/// when another channel already follows that address, and the answer says so:
/// the channel then stays where it was, since two rows cannot be one address.
pub(crate) async fn move_channel(
    pool: &SqlitePool,
    channel_id: i64,
    feed_key: &str,
    feed_url: &str,
) -> Result<bool, sqlx::Error> {
    let moved = sqlx::query(
        "UPDATE podcast_channel
            SET feed_key = ?1, feed_url = ?2, updated_at = ?3
          WHERE id = ?4
            AND NOT EXISTS (
                SELECT 1 FROM podcast_channel WHERE feed_key = ?1 AND id <> ?4
            )",
    )
    .bind(feed_key)
    .bind(feed_url)
    .bind(Utc::now().timestamp_millis())
    .bind(channel_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(moved > 0)
}

/// A refresh the host answered with a 304: nothing changed, and that is news.
pub(crate) async fn record_unchanged(
    pool: &SqlitePool,
    channel_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE podcast_channel SET last_refreshed_at = ?, last_error = NULL WHERE id = ?")
        .bind(Utc::now().timestamp_millis())
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// A refresh that failed: `code` is a word from `FetchFailure::code`. The channel
/// keeps what it had, and `last_refreshed_at` keeps saying when that was.
pub(crate) async fn record_failure(
    pool: &SqlitePool,
    channel_id: i64,
    code: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE podcast_channel SET last_error = ? WHERE id = ?")
        .bind(code)
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub(crate) async fn record_artwork(
    pool: &SqlitePool,
    channel_id: i64,
    hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE podcast_channel SET artwork_hash = ? WHERE id = ?")
        .bind(hash)
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The website a channel's feed names, if it is one a browser may be sent to:
/// the parser only keeps such an address, and this holds for a row it did not
/// write as well. Goes no further than the command that opens it.
pub(crate) async fn site_to_open(
    pool: &SqlitePool,
    channel_id: i64,
) -> Result<Option<Url>, sqlx::Error> {
    let site: Option<Option<String>> =
        sqlx::query_scalar("SELECT site_url FROM podcast_channel WHERE id = ?")
            .bind(channel_id)
            .fetch_optional(pool)
            .await?;
    Ok(site.flatten().as_deref().and_then(parse_address))
}

/// What playing an episode needs of its row and its channel's. Holds addresses,
/// and stays in the backend.
#[derive(Debug, Clone, PartialEq, FromRow)]
pub(crate) struct PlayableEpisode {
    pub id: i64,
    pub channel_id: i64,
    pub title: String,
    pub duration_ms: Option<i64>,
    pub position_ms: i64,
    pub played_at: Option<i64>,
    pub enclosure_url: Option<String>,
    pub enclosure_type: Option<String>,
    pub download_file: Option<String>,
    pub channel_title: String,
    pub feed_url: Option<String>,
    pub artwork_hash: Option<String>,
    pub playback_speed: Option<f64>,
}

pub(crate) async fn playable(
    pool: &SqlitePool,
    episode_id: i64,
) -> Result<Option<PlayableEpisode>, sqlx::Error> {
    sqlx::query_as(
        "SELECT e.id, e.channel_id, e.title, e.duration_ms, e.position_ms, e.played_at,
                e.enclosure_url, e.enclosure_type, e.download_file,
                c.title AS channel_title, c.feed_url, c.artwork_hash, c.playback_speed
           FROM podcast_episode e
           JOIN podcast_channel c ON c.id = e.channel_id
          WHERE e.id = ? AND e.deleted_at IS NULL",
    )
    .bind(episode_id)
    .fetch_optional(pool)
    .await
}

/// Where an episode's chapters may come from. Holds addresses, and stays in the
/// backend.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub(crate) struct ChapterSources {
    pub chapters_url: Option<String>,
    pub inline_chapters: Option<String>,
    pub enclosure_url: Option<String>,
    pub enclosure_type: Option<String>,
    pub download_file: Option<String>,
    pub feed_url: Option<String>,
    pub chapters_checked_at: Option<i64>,
    pub chapters_cache: Option<String>,
}

pub(crate) async fn chapter_sources(
    pool: &SqlitePool,
    episode_id: i64,
) -> Result<Option<ChapterSources>, sqlx::Error> {
    sqlx::query_as(
        "SELECT e.chapters_url, e.inline_chapters, e.enclosure_url, e.enclosure_type,
                e.download_file, c.feed_url, e.chapters_checked_at, e.chapters_cache
           FROM podcast_episode e
           JOIN podcast_channel c ON c.id = e.channel_id
          WHERE e.id = ? AND e.deleted_at IS NULL",
    )
    .bind(episode_id)
    .fetch_optional(pool)
    .await
}

/// What was found, or `None` for nothing, and when. Written only while the row still
/// names the sources it was found through: a refresh that changed them meanwhile
/// has just said this is stale.
pub(crate) async fn record_chapters(
    pool: &SqlitePool,
    episode_id: i64,
    sources: &ChapterSources,
    cache: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE podcast_episode
            SET chapters_cache = ?, chapters_checked_at = ?
          WHERE id = ?
            AND chapters_url IS ? AND inline_chapters IS ? AND enclosure_url IS ?",
    )
    .bind(cache)
    .bind(Utc::now().timestamp_millis())
    .bind(episode_id)
    .bind(&sources.chapters_url)
    .bind(&sources.inline_chapters)
    .bind(&sources.enclosure_url)
    .execute(pool)
    .await?;
    Ok(())
}

/// Where an episode's transcript may come from. Holds addresses, and stays in the
/// backend. Read apart from everything else: a kept one may be megabytes.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub(crate) struct TranscriptSources {
    pub transcripts: Option<String>,
    pub feed_url: Option<String>,
    pub transcript_kind: Option<String>,
    pub transcript_cache: Option<String>,
}

pub(crate) async fn transcript_sources(
    pool: &SqlitePool,
    episode_id: i64,
) -> Result<Option<TranscriptSources>, sqlx::Error> {
    sqlx::query_as(
        "SELECT e.transcripts, c.feed_url, e.transcript_kind, e.transcript_cache
           FROM podcast_episode e
           JOIN podcast_channel c ON c.id = e.channel_id
          WHERE e.id = ? AND e.deleted_at IS NULL",
    )
    .bind(episode_id)
    .fetch_optional(pool)
    .await
}

/// Keeps a transcript with its episode, under the same condition as
/// [`record_chapters`].
pub(crate) async fn record_transcript(
    pool: &SqlitePool,
    episode_id: i64,
    transcripts: &str,
    kind: &str,
    text: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE podcast_episode
            SET transcript_kind = ?, transcript_cache = ?, transcript_checked_at = ?
          WHERE id = ? AND transcripts IS ?",
    )
    .bind(kind)
    .bind(text)
    .bind(Utc::now().timestamp_millis())
    .bind(episode_id)
    .bind(transcripts)
    .execute(pool)
    .await?;
    Ok(())
}

/// What a download needs of an episode.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub(crate) struct EpisodeDownload {
    pub channel_id: i64,
    pub title: String,
    pub enclosure_url: Option<String>,
    /// The address its channel was typed with, which decides what the enclosure
    /// may reach.
    pub feed_url: Option<String>,
    pub download_file: Option<String>,
}

/// An episode that can still be asked for: one the user removed is gone.
pub(crate) async fn episode_download(
    pool: &SqlitePool,
    episode_id: i64,
) -> Result<Option<EpisodeDownload>, sqlx::Error> {
    sqlx::query_as(
        "SELECT e.channel_id, e.title, e.enclosure_url, c.feed_url, e.download_file
           FROM podcast_episode e
           JOIN podcast_channel c ON c.id = e.channel_id
          WHERE e.id = ? AND e.deleted_at IS NULL",
    )
    .bind(episode_id)
    .fetch_optional(pool)
    .await
}

/// One row of a channel's queue.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub(crate) struct QueueRow {
    pub id: i64,
    pub channel_id: i64,
    pub title: String,
    pub duration_ms: Option<i64>,
    pub has_enclosure: bool,
    pub downloaded: bool,
}

/// A channel's episodes in the page's order: newest first, undated ones last.
pub(crate) async fn queue_rows(
    pool: &SqlitePool,
    channel_id: i64,
) -> Result<Vec<QueueRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, channel_id, title, duration_ms,
                enclosure_url IS NOT NULL AS has_enclosure,
                download_file IS NOT NULL AS downloaded
           FROM podcast_episode
          WHERE channel_id = ? AND deleted_at IS NULL
          ORDER BY published_at IS NULL, published_at DESC, id DESC",
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await
}

/// Whether the end of the episode before it should pass this one over: heard
/// already, or gone. A queue lives in memory, and a row may have been removed since
/// it was made.
pub(crate) async fn nothing_to_hear(
    pool: &SqlitePool,
    episode_id: i64,
) -> Result<bool, sqlx::Error> {
    let heard: Option<bool> = sqlx::query_scalar(
        "SELECT played_at IS NOT NULL OR deleted_at IS NOT NULL
           FROM podcast_episode WHERE id = ?",
    )
    .bind(episode_id)
    .fetch_optional(pool)
    .await?;
    Ok(heard.unwrap_or(true))
}

pub(crate) async fn note_opened(pool: &SqlitePool, episode_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE podcast_episode SET last_opened_at = ? WHERE id = ?")
        .bind(Utc::now().timestamp_millis())
        .bind(episode_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Where an episode stands. Played, the position goes back to zero, since a played
/// episode starts over whatever was stored. Whether this is what made it played is
/// the answer. A row marked played keeps that mark while it is heard again: only a
/// person takes it off.
pub(crate) async fn save_position(
    pool: &SqlitePool,
    episode_id: i64,
    position_ms: u64,
    played: bool,
) -> Result<bool, sqlx::Error> {
    let now = Utc::now().timestamp_millis();
    let position = if played {
        0
    } else {
        i64::try_from(position_ms).unwrap_or(i64::MAX)
    };
    let flipped = sqlx::query_scalar::<_, i64>(
        "UPDATE podcast_episode
            SET position_ms = ?1,
                played_at = CASE WHEN ?2 THEN COALESCE(played_at, ?3) ELSE played_at END
          WHERE id = ?4
          RETURNING played_at IS NOT NULL AND played_at = ?3",
    )
    .bind(position)
    .bind(played)
    .bind(now)
    .bind(episode_id)
    .fetch_optional(pool)
    .await?;
    Ok(played && flipped == Some(1))
}

/// Marked by hand. Either way the position is forgotten: played starts over, and
/// unplayed is asked to be new again. The channel it belongs to is the answer, or
/// `None` when there is no such row.
pub(crate) async fn mark_played(
    pool: &SqlitePool,
    episode_id: i64,
    played: bool,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE podcast_episode
            SET position_ms = 0,
                played_at = CASE WHEN ?1 THEN COALESCE(played_at, ?2) ELSE NULL END
          WHERE id = ?3 AND deleted_at IS NULL
          RETURNING channel_id",
    )
    .bind(played)
    .bind(Utc::now().timestamp_millis())
    .bind(episode_id)
    .fetch_optional(pool)
    .await
}

/// [`mark_played`] for a whole channel, in one statement. How many rows it touched
/// is the answer.
pub(crate) async fn mark_channel_played(
    pool: &SqlitePool,
    channel_id: i64,
    played: bool,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE podcast_episode
            SET position_ms = 0,
                played_at = CASE WHEN ?1 THEN COALESCE(played_at, ?2) ELSE NULL END
          WHERE channel_id = ?3 AND deleted_at IS NULL",
    )
    .bind(played)
    .bind(Utc::now().timestamp_millis())
    .bind(channel_id)
    .execute(pool)
    .await?
    .rows_affected())
}

/// One episode per channel, across channels (decision 7): the one in progress, the
/// most recently opened if several are, else the newest unplayed. Channels with one
/// in progress first. Only what can play: a restored profile's episodes have no
/// address until the feed is subscribed to again. Reads the two tables and nothing
/// else.
pub(crate) async fn to_listen(pool: &SqlitePool) -> Result<Vec<PodcastEpisode>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{EPISODE_SELECT}
          WHERE id IN (
            SELECT id FROM (
              SELECT id,
                     ROW_NUMBER() OVER (
                       PARTITION BY channel_id
                       ORDER BY position_ms > 0 DESC,
                                CASE WHEN position_ms > 0 THEN last_opened_at END DESC,
                                published_at IS NULL, published_at DESC, id DESC
                     ) AS place
                FROM podcast_episode
               WHERE deleted_at IS NULL
                 AND played_at IS NULL
                 AND (enclosure_url IS NOT NULL OR download_file IS NOT NULL)
            ) WHERE place = 1
          )
          ORDER BY position_ms > 0 DESC,
                   CASE WHEN position_ms > 0 THEN last_opened_at END DESC,
                   published_at IS NULL, published_at DESC, id DESC"
    )))
    .fetch_all(pool)
    .await
}

/// `None` is the normal speed. Whether a row was there is the answer.
pub(crate) async fn set_channel_speed(
    pool: &SqlitePool,
    channel_id: i64,
    speed: Option<f64>,
) -> Result<bool, sqlx::Error> {
    let updated = sqlx::query("UPDATE podcast_channel SET playback_speed = ? WHERE id = ?")
        .bind(speed)
        .bind(channel_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(updated > 0)
}

/// The quota in bytes. A value nobody could have meant is no answer.
pub(crate) async fn download_quota_bytes(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(DOWNLOAD_QUOTA_KEY)
            .fetch_optional(pool)
            .await?;
    let gigabytes = value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|gigabytes| *gigabytes >= 1)
        .unwrap_or(DEFAULT_DOWNLOAD_QUOTA_GB);
    Ok(gigabytes.saturating_mul(GIB))
}

/// What the kept episodes weigh, read from the rows: a stray file in the folder
/// is not a download. `except` leaves out the episode being fetched again, whose
/// old file is gone. A negative size counts for nothing: a row may have come from
/// an archive somebody made, and it must not buy room under the quota.
pub(crate) async fn download_used_bytes(
    pool: &SqlitePool,
    except: Option<i64>,
) -> Result<u64, sqlx::Error> {
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(MAX(download_size, 0)), 0) FROM podcast_episode
          WHERE download_file IS NOT NULL AND id IS NOT ?",
    )
    .bind(except)
    .fetch_one(pool)
    .await?;
    Ok(used.max(0) as u64)
}

/// Records a file put in place. False, with nothing written, when the episode is
/// gone: the channel was unsubscribed from while the body was being read.
pub(crate) async fn record_download(
    pool: &SqlitePool,
    episode_id: i64,
    file: &str,
    size: u64,
) -> Result<bool, sqlx::Error> {
    let updated = sqlx::query(
        "UPDATE podcast_episode
            SET download_file = ?, download_size = ?, downloaded_at = ?
          WHERE id = ?",
    )
    .bind(file)
    .bind(size as i64)
    .bind(Utc::now().timestamp_millis())
    .bind(episode_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(updated > 0)
}

/// The episode has no file any more, and keeps its row.
pub(crate) async fn clear_download(pool: &SqlitePool, episode_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE podcast_episode
            SET download_file = NULL, download_size = NULL, downloaded_at = NULL
          WHERE id = ?",
    )
    .bind(episode_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn download_file_of(
    pool: &SqlitePool,
    episode_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    let file: Option<Option<String>> =
        sqlx::query_scalar("SELECT download_file FROM podcast_episode WHERE id = ?")
            .bind(episode_id)
            .fetch_optional(pool)
            .await?;
    Ok(file.flatten())
}

/// `(episode, file name)` of every download this profile holds.
pub(crate) async fn all_downloads(pool: &SqlitePool) -> Result<Vec<(i64, String)>, sqlx::Error> {
    sqlx::query_as("SELECT id, download_file FROM podcast_episode WHERE download_file IS NOT NULL")
        .fetch_all(pool)
        .await
}

/// The file names one channel's episodes hold, read before the channel goes.
pub(crate) async fn downloads_of_channel(
    pool: &SqlitePool,
    channel_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT download_file FROM podcast_episode
          WHERE channel_id = ? AND download_file IS NOT NULL",
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await
}

/// Forgets a channel and, by the foreign key, its episodes. Whether a row was
/// there to forget is the answer.
pub(crate) async fn delete_channel(
    pool: &SqlitePool,
    channel_id: i64,
) -> Result<bool, sqlx::Error> {
    let deleted = sqlx::query("DELETE FROM podcast_channel WHERE id = ?")
        .bind(channel_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(deleted > 0)
}
