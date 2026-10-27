//! Persistent playback queue.
//!
//! The queue lives in the per-profile `data.db` via the `queue_item`
//! table (populated by [`fill_queue`]) and the `queue.current_index`
//! row of `profile_setting` (pointer to the active slot).
//!
//! Playing a track from the library view fills the whole queue with
//! the current view, sets `current_index` to the clicked row, and the
//! audio engine's auto-advance task walks forward through the queue
//! as each track ends. Shuffle / repeat behaviour is applied here,
//! not in the decoder thread.
//!
//! None of these functions touch the audio engine — they're pure DB
//! operations that return [`QueueTrack`]s for the caller to feed into
//! `AudioCmd::LoadAndPlay`.
//!
//! `dead_code` is tolerated module-wide because shuffle / unshuffle /
//! restore_state / persist_resume_point are consumed by later
//! checkpoints (12 = shuffle+repeat, 13 = startup restore).

#![allow(dead_code)]

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};

use crate::{commands::player::PlayerStateSnapshot, error::AppResult};

mod edit;
mod remove;
mod shuffle;

pub use edit::{append, append_to_user_queue, fill_queue, insert_after_current, reorder};
pub use remove::{clear, position_of_queue_id, remove_by_queue_id, remove_range};
pub use shuffle::{shuffle, shuffle_by_album, unshuffle};

/// Minimum track shape needed to hand off to the decoder thread. Kept
/// narrower than [`crate::commands::track::Track`] because playback
/// doesn't need the full metadata block; anything the UI wants on top
/// is fetched via `list_tracks`.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct QueueTrack {
    pub id: i64,
    pub file_path: String,
    pub duration_ms: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub album_title: Option<String>,
    /// The album row, not just its title. Every per-album choice the
    /// user makes (a hand-set motion cover, "go to album") is keyed by
    /// id; with only the title on the playing track, the now-playing
    /// surfaces could not reach any of them (#766).
    pub album_id: Option<i64>,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    /// Audio quality fields, surfaced on the PlayerBar footer and on
    /// the Hi-Res badge overlays. `None` when the scanner couldn't
    /// extract them (lossy formats often skip bit_depth, very old
    /// files may miss bitrate, …).
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub bit_depth: Option<i64>,
    pub codec: Option<String>,
    pub file_size: i64,
}

impl QueueTrack {
    /// Return the absolute filesystem path the decoder should open.
    pub fn as_path(&self) -> PathBuf {
        PathBuf::from(&self.file_path)
    }
}

/// Direction arguments for [`advance`].
#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Next,
    Previous,
}

/// Repeat mode. Mirrors the `player.repeat_mode` profile_setting
/// string ('off' / 'all' / 'one').
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "all" => Self::All,
            "one" => Self::One,
            _ => Self::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }
}

/// How shuffle reorders the queue. Mirrors the `player.shuffle_mode`
/// profile_setting string.
///
/// Three-way rather than the boolean it replaces, because "shuffle" was
/// answering two different questions with one switch (#618). Shuffling
/// individual tracks is right for a playlist; on a library full of
/// records it takes every album apart, which is the opposite of what
/// someone who listens to albums wants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShuffleMode {
    /// The queue plays in the order it was built.
    #[default]
    Off,
    /// Individual tracks, in no relation to each other.
    Tracks,
    /// Whole records. Only the order of the albums is randomised —
    /// inside each one the tracks keep disc and track order.
    Albums,
}

impl ShuffleMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "tracks" => Self::Tracks,
            "albums" => Self::Albums,
            _ => Self::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Tracks => "tracks",
            Self::Albums => "albums",
        }
    }

    /// Whether anything is being shuffled at all.
    ///
    /// This is the whole of what the older `player.shuffle` boolean
    /// could say, and what MPD's `random` flag can carry.
    pub fn is_on(self) -> bool {
        self != Self::Off
    }

    /// The form the decoder thread reads, out of a plain integer
    /// atomic. Nothing but
    /// [`SharedPlayback`](crate::audio::state::SharedPlayback) should
    /// care what number a mode is.
    pub fn as_bits(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Tracks => 1,
            Self::Albums => 2,
        }
    }

    /// Inverse of [`Self::as_bits`], total on purpose: an unknown
    /// number means the atomic was written by code this build does not
    /// have, and `Off` is a better answer than a panic on the audio
    /// path.
    pub fn from_bits(bits: u8) -> Self {
        match bits {
            1 => Self::Tracks,
            2 => Self::Albums,
            _ => Self::Off,
        }
    }

    /// The grouping half, as persisted. `Off` has none — turning
    /// shuffle off is not a third way of grouping, it is the absence
    /// of grouping, and the flavour the listener picked is kept so it
    /// survives an off/on.
    fn grouping(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Tracks => Some("tracks"),
            Self::Albums => Some("albums"),
        }
    }
}

// ---------------------------------------------------------------------
// Helpers: typed wrappers around profile_setting string values
// ---------------------------------------------------------------------

async fn read_setting_string(pool: &SqlitePool, key: &str) -> AppResult<Option<String>> {
    let row: Option<String> = sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

async fn read_setting_i64(pool: &SqlitePool, key: &str) -> AppResult<Option<i64>> {
    match read_setting_string(pool, key).await? {
        Some(s) => Ok(s.parse::<i64>().ok()),
        None => Ok(None),
    }
}

/// Generic over the executor so a caller that needs several rows to land
/// together can pass its transaction instead of the pool.
async fn write_setting_i64<'e, E>(executor: E, key: &str, value: i64) -> AppResult<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = ?",
    )
    .bind(value.to_string())
    .bind(now)
    .bind(key)
    .execute(executor)
    .await?;
    Ok(())
}

async fn write_setting_string(pool: &SqlitePool, key: &str, value: &str) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------
// Read helpers used by player commands + analytics
// ---------------------------------------------------------------------

pub async fn read_repeat_mode(pool: &SqlitePool) -> RepeatMode {
    match read_setting_string(pool, "player.repeat_mode").await {
        Ok(Some(s)) => RepeatMode::from_str(&s),
        _ => RepeatMode::Off,
    }
}

pub async fn read_shuffle(pool: &SqlitePool) -> bool {
    read_shuffle_mode(pool).await.is_on()
}

/// The shuffle state in force: the existing on/off switch crossed
/// with the grouping the listener picked.
///
/// Kept as two rows rather than one three-valued one, for three
/// reasons. `player.shuffle` is still what MPD's `random` flag maps
/// onto and what a build without album shuffle reads, so it stays
/// authoritative for "is shuffle on". The grouping is remembered
/// while shuffle is off, so someone who listens to records does not
/// have to re-pick it every time they turn shuffle back on. And a
/// profile that predates this upgrade needs no migration: no grouping
/// row means tracks, which is what shuffle has always done.
pub async fn read_shuffle_mode(pool: &SqlitePool) -> ShuffleMode {
    let on = matches!(
        read_setting_string(pool, "player.shuffle").await,
        Ok(Some(ref s)) if s == "true"
    );
    if !on {
        return ShuffleMode::Off;
    }
    match read_shuffle_grouping_preference(pool).await {
        ShuffleMode::Off => ShuffleMode::Tracks,
        grouping => grouping,
    }
}

/// The remembered grouping on its own, ignoring whether shuffle is on.
/// Never `Off` in practice — an absent or unreadable row reads as
/// `Tracks`, which is what shuffle did before album grouping existed.
///
/// This is what "turn shuffle on" means for every surface that can
/// only say on: the player button, an MPD `random 1`, the Shuffle
/// action on an album or a playlist.
pub async fn read_shuffle_grouping_preference(pool: &SqlitePool) -> ShuffleMode {
    match read_setting_string(pool, "player.shuffle_grouping").await {
        Ok(Some(ref s)) => match ShuffleMode::from_str(s) {
            ShuffleMode::Off => ShuffleMode::Tracks,
            grouping => grouping,
        },
        _ => ShuffleMode::Tracks,
    }
}

pub async fn write_repeat_mode(pool: &SqlitePool, mode: RepeatMode) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = 'player.repeat_mode'",
    )
    .bind(mode.as_str())
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Persist a shuffle state: the switch always, the grouping only when
/// there is one to record.
///
/// The only setter, on purpose. A boolean one used to sit next to it
/// and now has no callers — and leaving it would have been a trap
/// rather than a convenience: `write_shuffle(pool, true)` forces the
/// `Tracks` grouping, so a future caller reaching for the obvious name
/// would silently throw away a listener's preference for whole
/// records. Turning shuffle on without choosing a grouping is
/// [`read_shuffle_grouping_preference`] followed by this.
///
/// Turning shuffle off deliberately leaves `player.shuffle_grouping`
/// alone — see [`read_shuffle_mode`] for why the listener's choice is
/// worth remembering.
pub async fn write_shuffle_mode(pool: &SqlitePool, mode: ShuffleMode) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    if let Some(grouping) = mode.grouping() {
        // Upserted rather than updated: this key is new, so no
        // migration seeded a row for it to update.
        sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('player.shuffle_grouping', ?, 'string', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(grouping)
        .bind(now)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = 'player.shuffle'",
    )
    .bind(if mode.is_on() { "true" } else { "false" })
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Reorder the queue to match `mode`. The one place that knows which
/// of the three orderings a mode means, so the toggle, the MPD
/// `random` command and [`crate::commands::player::player_play_tracks`]
/// cannot drift on it.
pub async fn apply_shuffle_mode(pool: &SqlitePool, mode: ShuffleMode) -> AppResult<()> {
    match mode {
        ShuffleMode::Off => unshuffle(pool).await,
        ShuffleMode::Tracks => shuffle(pool).await,
        ShuffleMode::Albums => shuffle_by_album(pool).await,
    }
}

/// Whether the persisted queue is whole records in their own order
/// (`queue.album_ordered`).
///
/// Read once per profile load and mirrored into the engine, the way the
/// shuffle mode is. An absent or unreadable row reads as `false`:
/// telling a listener their tracks are an album is the answer that
/// changes the gain, so it is the one that has to be asked for.
/// Record what the queue that now exists is, inside the transaction
/// that built it.
///
/// Every path that **replaces** the queue calls this, and there are
/// three of them: [`fill_queue`], and the empty-queue branches of
/// [`insert_after_current`] and [`append_to_user_queue`], each of which
/// builds a queue where there was none. A path that only inserts into
/// an existing queue leaves it alone — adding a track to a record
/// playing through does not stop it being one.
async fn write_album_ordered(
    tx: &mut sqlx::SqliteConnection,
    album_ordered: bool,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
              VALUES ('queue.album_ordered', ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                        updated_at = excluded.updated_at",
    )
    .bind(if album_ordered { "true" } else { "false" })
    .bind(now)
    .execute(tx)
    .await?;
    Ok(())
}

pub async fn read_album_ordered(pool: &SqlitePool) -> bool {
    matches!(
        read_setting_string(pool, "queue.album_ordered").await,
        Ok(Some(ref value)) if value == "true"
    )
}

/// The queue cursor (`queue.current_index`), normalized to a valid index:
/// clamped into `[0, len)`, or `0` when the queue is empty or the stored
/// value is unset / out of range. The single source of truth for "which
/// entry is current" that the MPD `status` / `currentsong` handlers read —
/// a raw stored value can dangle past the end after the queue shrinks.
pub async fn current_index(pool: &SqlitePool) -> i64 {
    let len = queue_length(pool).await.unwrap_or(0);
    if len <= 0 {
        return 0;
    }
    let raw = read_setting_i64(pool, "queue.current_index")
        .await
        .ok()
        .flatten()
        .unwrap_or(0);
    raw.clamp(0, len - 1)
}

// ---------------------------------------------------------------------
// Core queue operations
// ---------------------------------------------------------------------

/// Count of rows in `queue_item`. Used by [`advance`] to bound the
/// cursor when the queue length shrinks (e.g. a track is deleted).
pub async fn queue_length(pool: &SqlitePool) -> AppResult<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_item")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Fetch the full queue as an ordered `Vec<QueueTrack>`. Joined
/// with track / album / artist / artwork so the frontend doesn't
/// have to issue N extra queries to render the panel.
pub async fn list_queue(pool: &SqlitePool) -> AppResult<Vec<QueueTrack>> {
    let rows = sqlx::query_as::<_, QueueTrack>(
        r#"
        SELECT t.id,
               t.file_path,
               t.duration_ms,
               t.title,
               t.primary_artist AS artist_id,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                  SELECT ar2.name FROM track_artist ta2
                  JOIN artist ar2 ON ar2.id = ta2.artist_id
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_name,
               (SELECT GROUP_CONCAT(id, ',') FROM (
                  SELECT ta2.artist_id AS id FROM track_artist ta2
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_ids,
               al.title AS album_title,
               t.album_id,
               aw.hash  AS artwork_hash,
               aw.format AS artwork_format,
               t.bitrate, t.sample_rate, t.channels,
               t.bit_depth, t.codec,
               t.file_size
          FROM queue_item q
          JOIN track t       ON t.id = q.track_id
          LEFT JOIN album al  ON al.id = t.album_id
          LEFT JOIN artist ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         ORDER BY q.position
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Fetch the track at a specific position in the queue.
async fn track_at_position(pool: &SqlitePool, position: i64) -> AppResult<Option<QueueTrack>> {
    let row = sqlx::query_as::<_, QueueTrack>(
        r#"
        SELECT t.id,
               t.file_path,
               t.duration_ms,
               t.title,
               t.primary_artist AS artist_id,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                  SELECT ar2.name FROM track_artist ta2
                  JOIN artist ar2 ON ar2.id = ta2.artist_id
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_name,
               (SELECT GROUP_CONCAT(id, ',') FROM (
                  SELECT ta2.artist_id AS id FROM track_artist ta2
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_ids,
               al.title AS album_title,
               t.album_id,
               aw.hash  AS artwork_hash,
               aw.format AS artwork_format,
               t.bitrate, t.sample_rate, t.channels,
               t.bit_depth, t.codec,
               t.file_size
          FROM queue_item q
          JOIN track t       ON t.id = q.track_id
          LEFT JOIN album al  ON al.id = t.album_id
          LEFT JOIN artist ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE q.position = ?
         LIMIT 1
        "#,
    )
    .bind(position)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Return the track currently pointed at by `queue.current_index`.
///
/// The cursor is clamped into the queue first, as [`stepped_index`] does:
/// a stale one — a queue that shrank under it — would otherwise point
/// past the last row and report no track at all.
pub async fn current_track(pool: &SqlitePool) -> AppResult<Option<QueueTrack>> {
    let Some(idx) = read_setting_i64(pool, "queue.current_index").await? else {
        return Ok(None);
    };
    let length = queue_length(pool).await?;
    if length == 0 {
        return Ok(None);
    }
    track_at_position(pool, idx.clamp(0, length - 1)).await
}

/// Move the cursor to an arbitrary position in the existing queue
/// and return the track there. Used when the user double-clicks a
/// row in the QueuePanel to jump.
pub async fn jump_to(pool: &SqlitePool, position: i64) -> AppResult<Option<QueueTrack>> {
    let length = queue_length(pool).await?;
    if length == 0 {
        return Ok(None);
    }
    let clamped = position.clamp(0, length - 1);
    write_setting_i64(pool, "queue.current_index", clamped).await?;
    track_at_position(pool, clamped).await
}

/// What an absolute jump *would* land on, **without moving the cursor**
/// — the [`peek_step`] sibling for [`jump_to`], and the same reason
/// (#632).
pub async fn peek_jump(pool: &SqlitePool, position: i64) -> AppResult<Option<(i64, QueueTrack)>> {
    let length = queue_length(pool).await?;
    if length == 0 {
        return Ok(None);
    }
    let clamped = position.clamp(0, length - 1);
    Ok(track_at_position(pool, clamped)
        .await?
        .map(|track| (clamped, track)))
}

/// Where a step lands, or `None` when it runs off the end with repeat
/// off. Pure, so the rule is testable without a database.
///
/// `current` is clamped into the queue first: a stale
/// `queue.current_index` — a queue that shrank under it — would
/// otherwise step from a row that no longer exists.
pub(crate) fn stepped_index(
    length: i64,
    current: i64,
    direction: Direction,
    repeat: RepeatMode,
) -> Option<i64> {
    if length <= 0 {
        return None;
    }
    let current = current.clamp(0, length - 1);
    match (direction, repeat) {
        // Repeat-one re-plays the same slot regardless of direction.
        (_, RepeatMode::One) => Some(current),
        (Direction::Next, RepeatMode::Off) => {
            if current + 1 >= length {
                None
            } else {
                Some(current + 1)
            }
        }
        (Direction::Next, RepeatMode::All) => Some((current + 1) % length),
        (Direction::Previous, RepeatMode::Off) => Some((current - 1).max(0)),
        (Direction::Previous, RepeatMode::All) => Some(if current == 0 {
            length - 1
        } else {
            current - 1
        }),
    }
}

/// What a next / previous step *would* land on, **without moving the
/// cursor**: the index and the track there.
///
/// Split out of [`advance`] for the callers that must not write anything
/// until they know their load will be the one that plays (#632). They
/// peek, claim the dispatch, then [`commit_index`]. A caller with nothing
/// to arbitrate keeps using `advance`.
pub async fn peek_step(
    pool: &SqlitePool,
    direction: Direction,
    repeat: RepeatMode,
) -> AppResult<Option<(i64, QueueTrack)>> {
    let length = queue_length(pool).await?;
    let current = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0);
    let Some(index) = stepped_index(length, current, direction, repeat) else {
        return Ok(None);
    };
    Ok(track_at_position(pool, index)
        .await?
        .map(|track| (index, track)))
}

/// Move the cursor to an index [`peek_step`] or [`stepped_index`] already
/// resolved. Writes nothing else.
pub async fn commit_index(pool: &SqlitePool, index: i64) -> AppResult<()> {
    write_setting_i64(pool, "queue.current_index", index).await
}

/// Apply a next / previous step to the queue cursor respecting the
/// repeat mode. Returns the newly-current track, or `None` if the
/// queue is empty / the step runs off the end with `RepeatMode::Off`.
///
/// The cursor is clamped to `[0, queue_length - 1]` before writing.
pub async fn advance(
    pool: &SqlitePool,
    direction: Direction,
    repeat: RepeatMode,
) -> AppResult<Option<QueueTrack>> {
    let Some((index, track)) = peek_step(pool, direction, repeat).await? else {
        return Ok(None);
    };
    commit_index(pool, index).await?;
    Ok(Some(track))
}

/// Non-mutating sibling of [`advance`]: returns what the next track
/// *would* be without moving the cursor. Used by the crossfade
/// prefetcher so it can hand the decoder a candidate without
/// committing to a queue advance — the cursor is bumped only when
/// the crossfade actually starts.
pub async fn peek_next(pool: &SqlitePool, repeat: RepeatMode) -> AppResult<Option<QueueTrack>> {
    let length = queue_length(pool).await?;
    let current = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0);
    // The same rule as the step itself, so what is preloaded for a gapless
    // hand-over is what the advance then plays, a stale cursor included.
    let Some(next_index) = stepped_index(length, current, Direction::Next, repeat) else {
        return Ok(None);
    };
    track_at_position(pool, next_index).await
}

/// How close to the end a stored position has to be to count as "the
/// track was over".
const RESUME_TAIL_MS: u64 = 2_000;

/// Where to resume a track from its stored position (#898).
///
/// A position in the last two seconds starts the track over. That is a
/// track that was finished, or one that failed on its way out; resuming
/// it would play a blip and end, or, for a file that cannot decode its
/// tail, fail again within a millisecond on every press of Play. An
/// unknown duration (0) leaves the position alone.
pub(crate) fn resume_position(position_ms: u64, duration_ms: u64) -> u64 {
    if duration_ms > 0 && position_ms.saturating_add(RESUME_TAIL_MS) >= duration_ms {
        0
    } else {
        position_ms
    }
}

/// Startup restore: return the track + position the UI should show at
/// mount, without starting playback. Priority:
///
/// 1. `player.last_track_id` + `player.last_position_ms` if the track
///    still exists and is available,
/// 2. otherwise the current queue track at offset 0 ms,
/// 3. otherwise `None`.
pub async fn restore_state(pool: &SqlitePool) -> AppResult<Option<(QueueTrack, u64)>> {
    if let Some(last_id) = read_setting_i64(pool, "player.last_track_id").await? {
        if last_id > 0 {
            let row = sqlx::query_as::<_, QueueTrack>(
                r#"
                SELECT t.id, t.file_path, t.duration_ms, t.title,
                       t.primary_artist AS artist_id,
                       (SELECT GROUP_CONCAT(name, ', ') FROM (
                          SELECT ar2.name FROM track_artist ta2
                          JOIN artist ar2 ON ar2.id = ta2.artist_id
                          WHERE ta2.track_id = t.id
                          ORDER BY ta2.position
                       )) AS artist_name,
                       (SELECT GROUP_CONCAT(id, ',') FROM (
                          SELECT ta2.artist_id AS id FROM track_artist ta2
                          WHERE ta2.track_id = t.id
                          ORDER BY ta2.position
                       )) AS artist_ids,
                       al.title AS album_title,
                       t.album_id,
                       aw.hash AS artwork_hash,
                       aw.format AS artwork_format,
                       t.bitrate, t.sample_rate, t.channels,
                       t.bit_depth, t.codec,
                       t.file_size
                  FROM track t
                  LEFT JOIN album al ON al.id = t.album_id
                  LEFT JOIN artwork aw ON aw.id = al.artwork_id
                 WHERE t.id = ? AND t.is_available = 1
                "#,
            )
            .bind(last_id)
            .fetch_optional(pool)
            .await?;
            if let Some(track) = row {
                let pos = read_setting_i64(pool, "player.last_position_ms")
                    .await?
                    .unwrap_or(0)
                    .max(0) as u64;
                let pos = resume_position(pos, track.duration_ms.max(0) as u64);
                return Ok(Some((track, pos)));
            }
        }
    }
    match current_track(pool).await? {
        Some(t) => Ok(Some((t, 0))),
        None => Ok(None),
    }
}

/// Persist the last-playing track id + position so the next app
/// launch can resume from where the user stopped.
pub async fn persist_resume_point(
    pool: &SqlitePool,
    track_id: i64,
    position_ms: u64,
) -> AppResult<()> {
    // Both rows in one transaction. Three callers can be writing — the
    // ten-second ticker, the exit event and the device-error rebuild — and
    // a half-applied pair would name one track beside another's position.
    let mut tx = pool.begin().await?;
    write_setting_i64(&mut *tx, "player.last_track_id", track_id).await?;
    write_setting_i64(&mut *tx, "player.last_position_ms", position_ms as i64).await?;
    tx.commit().await?;
    Ok(())
}

// Re-export the player state snapshot type here so the analytics task
// can keep its imports minimal. Not a real runtime dependency.
#[allow(dead_code)]
fn _type_check() -> Option<PlayerStateSnapshot> {
    None
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod step_tests;

#[cfg(test)]
mod fill_queue_tests;
