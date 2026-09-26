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

use crate::{
    commands::player::PlayerStateSnapshot,
    error::{AppError, AppResult},
};

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

/// Clear the queue and insert new rows, one per track, with positions
/// 0..n. Also sets `queue.current_index` to `start_index`. Runs in a
/// single transaction so the UI never sees a partial state.
///
/// `album_ordered` says whether this list is whole records in their own
/// order — what a generator produces in album mode, and what no
/// `source_type` can express (#647). Persisted alongside the queue
/// rather than held in memory only, for the same reason the shuffle
/// mode is: a session survives a restart, and the gain decision has to
/// survive with it.
///
/// **Every replacement writes it**, including the ones that write
/// `false`. This is the single path that replaces the queue, which is
/// exactly what makes it the place a stale flag cannot outlive its
/// session.
pub async fn fill_queue(
    pool: &SqlitePool,
    source_type: &str,
    source_id: Option<i64>,
    track_ids: &[i64],
    start_index: usize,
    album_ordered: bool,
) -> AppResult<()> {
    if track_ids.is_empty() {
        return Err(AppError::Other(
            "cannot fill queue with empty track list".into(),
        ));
    }
    if start_index >= track_ids.len() {
        return Err(AppError::Other(format!(
            "start_index {start_index} out of range (queue length {})",
            track_ids.len()
        )));
    }

    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM queue_item")
        .execute(&mut *tx)
        .await?;

    let now = Utc::now().timestamp_millis();
    for (pos, track_id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(track_id)
        .bind(pos as i64)
        .bind(source_type)
        .bind(source_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = 'queue.current_index'",
    )
    .bind((start_index as i64).to_string())
    .bind(now)
    .execute(&mut *tx)
    .await?;

    write_album_ordered(&mut tx, album_ordered, now).await?;

    // New queue invalidates any previous shuffle snapshot.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Append `track_ids` to the end of the queue without disturbing the
/// current cursor. Used by the "Add to queue" context menu action so
/// the user can stack tracks without losing what's currently playing.
pub async fn append(
    pool: &SqlitePool,
    track_ids: &[i64],
    source_type: &str,
    source_id: Option<i64>,
) -> AppResult<()> {
    if track_ids.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    let max_pos: Option<i64> = sqlx::query_scalar("SELECT MAX(position) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;
    let start = max_pos.map(|p| p + 1).unwrap_or(0);
    let now = Utc::now().timestamp_millis();
    for (offset, id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(start + offset as i64)
        .bind(source_type)
        .bind(source_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    // Order changed → shuffle snapshot is no longer reusable.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Append `track_ids` to the **user queue** — the contiguous block of
/// `source_type = 'manual'` items sitting between the current track
/// and the context tail (whatever album / playlist / smart-playlist
/// fill seeded the queue). Matches Spotify semantics: "Add to queue"
/// stacks tracks at the *bottom* of the user queue, not at the
/// absolute end past every remaining album track, so the user can
/// keep playing the album they started while their manual picks fire
/// one by one in between.
///
/// Boundary = `MIN(position)` among items past the current cursor
/// whose `source_type != 'manual'`. NULL boundary means the entire
/// tail is already manual (or there's nothing past the cursor) — that
/// degenerates into [`append`]. Empty queue still degenerates into
/// [`fill_queue`] so the first "Add to queue" click on a fresh
/// session starts playback.
/// Returns whether the queue was **replaced** rather than added to —
/// true only on the empty-queue branch, which builds a new queue and so
/// decides afresh what it is. The caller mirrors that into the engine
/// (#647).
pub async fn append_to_user_queue(
    pool: &SqlitePool,
    track_ids: &[i64],
    source_id: Option<i64>,
) -> AppResult<bool> {
    if track_ids.is_empty() {
        return Ok(false);
    }

    // Single transaction wraps the boundary lookup AND the insert so the
    // decision can't be invalidated between reads and writes by a
    // concurrent advance / fill_queue / shuffle (SQLite WAL allows
    // many readers but only one writer, and our reads were happening
    // outside that single-writer envelope before this commit). Every
    // SQL inside the function now runs against `&mut *tx`.
    let mut tx = pool.begin().await?;
    let now = Utc::now().timestamp_millis();

    let len: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;

    // Empty queue — replicate `fill_queue` inline so it stays in the
    // same tx. Same shape, just bound to "manual" since we know the
    // origin.
    if len == 0 {
        for (pos, id) in track_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
                 VALUES (?, ?, 'manual', ?, ?)",
            )
            .bind(id)
            .bind(pos as i64)
            .bind(source_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE profile_setting
                SET value = ?, updated_at = ?
              WHERE key = 'queue.current_index'",
        )
        .bind("0")
        .bind(now)
        .execute(&mut *tx)
        .await?;
        // A queue built out of picks the listener stacked by hand is not
        // a session of records, whatever the last one was.
        write_album_ordered(&mut tx, false, now).await?;
        sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(true);
    }

    let current_raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = 'queue.current_index'")
            .fetch_optional(&mut *tx)
            .await?;
    let current = current_raw
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, len - 1);

    let boundary: Option<i64> = sqlx::query_scalar(
        "SELECT MIN(position) FROM queue_item
          WHERE position > ?
            AND source_type != 'manual'",
    )
    .bind(current)
    .fetch_one(&mut *tx)
    .await?;

    // NULL boundary = no context tail past the cursor (entire tail is
    // already manual, or current is at the very end). Same end shape as
    // `append`: drop the new rows at MAX(position) + 1.
    let insert_at = match boundary {
        Some(p) => p,
        None => {
            let max_pos: Option<i64> = sqlx::query_scalar("SELECT MAX(position) FROM queue_item")
                .fetch_one(&mut *tx)
                .await?;
            max_pos.map(|p| p + 1).unwrap_or(0)
        }
    };
    let needs_shift = boundary.is_some();

    let count = track_ids.len() as i64;

    if needs_shift {
        // Park-shift detour — same trick as `insert_after_current`.
        // SQLite checks UNIQUE(position) per row so a direct
        // `position + N` collides mid-update; bump the affected rows
        // to a high range, then bring them back down past the freshly
        // inserted block.
        const OFFSET: i64 = 10_000_000;
        sqlx::query("UPDATE queue_item SET position = position + ? WHERE position >= ?")
            .bind(OFFSET)
            .bind(insert_at)
            .execute(&mut *tx)
            .await?;

        for (offset, id) in track_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
                 VALUES (?, ?, 'manual', ?, ?)",
            )
            .bind(id)
            .bind(insert_at + offset as i64)
            .bind(source_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE queue_item SET position = position - ? + ? WHERE position >= ?")
            .bind(OFFSET)
            .bind(count)
            .bind(insert_at + OFFSET)
            .execute(&mut *tx)
            .await?;
    } else {
        // No shift needed — append directly past the last row.
        for (offset, id) in track_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
                 VALUES (?, ?, 'manual', ?, ?)",
            )
            .bind(id)
            .bind(insert_at + offset as i64)
            .bind(source_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
    }

    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(false)
}

/// Insert `track_ids` immediately after the current cursor position.
/// Existing items past the cursor are pushed down to keep the queue
/// dense. The cursor itself doesn't move, so the currently-playing
/// track keeps playing.
///
/// Returns whether the queue was **replaced** rather than inserted
/// into: an empty one is filled instead, which is a new queue and a new
/// answer to what it is made of. The caller mirrors that into the
/// engine (#647).
pub async fn insert_after_current(
    pool: &SqlitePool,
    track_ids: &[i64],
    source_type: &str,
    source_id: Option<i64>,
) -> AppResult<bool> {
    if track_ids.is_empty() {
        return Ok(false);
    }
    let len = queue_length(pool).await?;
    if len == 0 {
        // No queue yet — fall back to filling it and starting at 0.
        // An empty queue filled by "play next" is a hand-built list, not
        // a session of records — whatever the tracks happen to be.
        fill_queue(pool, source_type, source_id, track_ids, 0, false).await?;
        return Ok(true);
    }
    let current = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0)
        .clamp(0, len - 1);
    let insert_at = current + 1;
    let count = track_ids.len() as i64;
    let mut tx = pool.begin().await?;

    // Push existing items down to make room. SQLite checks the
    // UNIQUE(position) constraint per row, so a direct
    // `position = position + N` would collide mid-update. Bump the
    // affected rows into a high range first, then bring them back
    // down past the inserted block. The 10_000_000 offset is well
    // above any realistic queue length.
    const OFFSET: i64 = 10_000_000;
    sqlx::query("UPDATE queue_item SET position = position + ? WHERE position >= ?")
        .bind(OFFSET)
        .bind(insert_at)
        .execute(&mut *tx)
        .await?;

    let now = Utc::now().timestamp_millis();
    for (offset, id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(insert_at + offset as i64)
        .bind(source_type)
        .bind(source_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    // Bring the bumped rows back down past the freshly-inserted block.
    sqlx::query("UPDATE queue_item SET position = position - ? + ? WHERE position >= ?")
        .bind(OFFSET)
        .bind(count)
        .bind(insert_at + OFFSET)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    // The queue was not replaced, so what it is made of has not
    // changed. The inserted rows say `'manual'`, and a `'manual'` row
    // is never read as a record playing through whatever session it
    // landed in — see `SharedPlayback::listening_to`. Clearing the flag
    // here instead would take the album gain from every record still
    // queued behind the one track the listener slipped in.
    Ok(false)
}

/// Move the queue item at `from` to the slot at `to`, shifting the
/// items between them in the opposite direction so positions stay
/// dense and unique. Also adjusts `queue.current_index` so the
/// playing track keeps playing — only the user's drag of the very
/// item that's playing should change which position is "current".
///
/// `from` / `to` are clamped to `[0, len-1]`; out-of-range drops
/// snap to the nearest end. SQLite's UNIQUE(position) is honored by
/// parking the moved row at a high offset before the shift, then
/// dropping it back to the target slot.
pub async fn reorder(pool: &SqlitePool, from: i64, to: i64) -> AppResult<()> {
    let len = queue_length(pool).await?;
    if len == 0 {
        return Ok(());
    }
    let from = from.clamp(0, len - 1);
    let to = to.clamp(0, len - 1);
    if from == to {
        return Ok(());
    }

    const PARK: i64 = 10_000_000;
    let mut tx = pool.begin().await?;

    // 1. Park the moved row out of the way.
    sqlx::query("UPDATE queue_item SET position = ? WHERE position = ?")
        .bind(PARK)
        .bind(from)
        .execute(&mut *tx)
        .await?;

    // 2. Shift the affected range, again via PARK detour to keep the
    //    UNIQUE constraint from firing mid-update on a contiguous
    //    range — SQLite checks per row, so a direct
    //    `position = position ± 1` would collide on the first step.
    if to > from {
        // Items in (from, to] shift down by 1.
        sqlx::query(
            "UPDATE queue_item SET position = position + ?
              WHERE position > ? AND position <= ?",
        )
        .bind(PARK)
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE queue_item SET position = position - ? - 1
              WHERE position > ? AND position <= ?",
        )
        .bind(PARK)
        .bind(from + PARK)
        .bind(to + PARK)
        .execute(&mut *tx)
        .await?;
    } else {
        // Items in [to, from) shift up by 1.
        sqlx::query(
            "UPDATE queue_item SET position = position + ?
              WHERE position >= ? AND position < ?",
        )
        .bind(PARK)
        .bind(to)
        .bind(from)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE queue_item SET position = position - ? + 1
              WHERE position >= ? AND position < ?",
        )
        .bind(PARK)
        .bind(to + PARK)
        .bind(from + PARK)
        .execute(&mut *tx)
        .await?;
    }

    // 3. Drop the parked row at its target.
    sqlx::query("UPDATE queue_item SET position = ? WHERE position = ?")
        .bind(to)
        .bind(PARK)
        .execute(&mut *tx)
        .await?;

    // 4. Adjust the cursor so the currently-playing track keeps
    //    pointing at itself even when we shifted rows around it.
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = 'queue.current_index'")
            .fetch_optional(&mut *tx)
            .await?;
    let current = raw.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let new_current = if current == from {
        to
    } else if from < to && current > from && current <= to {
        current - 1
    } else if to < from && current >= to && current < from {
        current + 1
    } else {
        current
    };
    if new_current != current {
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "UPDATE profile_setting SET value = ?, updated_at = ?
              WHERE key = 'queue.current_index'",
        )
        .bind(new_current.to_string())
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    // Reordering invalidates the pre-shuffle snapshot — the original
    // order can't be reconstructed from a manually-tweaked queue.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

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
pub async fn current_track(pool: &SqlitePool) -> AppResult<Option<QueueTrack>> {
    let Some(idx) = read_setting_i64(pool, "queue.current_index").await? else {
        return Ok(None);
    };
    track_at_position(pool, idx).await
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
    if length == 0 {
        return Ok(None);
    }
    let current = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0);

    let next_index = match repeat {
        RepeatMode::One => current,
        RepeatMode::Off => {
            if current + 1 >= length {
                return Ok(None);
            }
            current + 1
        }
        RepeatMode::All => (current + 1) % length,
    };
    track_at_position(pool, next_index).await
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

// ---------------------------------------------------------------------
// Shuffle / unshuffle
// ---------------------------------------------------------------------

/// Randomize the queue, keeping the currently-playing track at
/// position 0. Stashes the pre-shuffle ordering in
/// `profile_setting['queue.preshuffle']` (JSON array of track ids, in
/// their original order) so [`unshuffle`] can restore it.
///
/// Fisher–Yates on the slice after the current track; this is cheap
/// and deterministic given the crate-level RNG.
pub async fn shuffle(pool: &SqlitePool) -> AppResult<()> {
    // Read current ordering.
    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT position, track_id FROM queue_item ORDER BY position")
            .fetch_all(pool)
            .await?;
    if rows.len() < 2 {
        return Ok(()); // nothing to shuffle
    }

    let current_index = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0)
        .clamp(0, rows.len() as i64 - 1) as usize;

    snapshot_preshuffle_order(pool, &rows.iter().map(|(_, id)| *id).collect::<Vec<_>>()).await?;

    // Build the new ordering: [current, ...shuffled rest].
    let mut ids: Vec<i64> = rows.iter().map(|(_, id)| *id).collect();
    let current_id = ids.remove(current_index);
    fisher_yates(&mut ids);
    let mut new_ids = Vec::with_capacity(ids.len() + 1);
    new_ids.push(current_id);
    new_ids.extend(ids);

    write_queue_order(pool, &new_ids, 0).await
}

/// Take the pre-shuffle snapshot, unless one is already held.
///
/// `queue.preshuffle` records the order the listener actually built,
/// so [`unshuffle`] can give it back. It must be taken **once**, on
/// the way into shuffle: re-taking it while already shuffled would
/// record the shuffled order as the original one, and turning shuffle
/// off would then restore the mess instead of undoing it.
///
/// That became reachable when shuffle grew a third state (#618) — you
/// can now go from grouping tracks to grouping albums without passing
/// through off. The queue-replacing paths clear the key, so "a
/// snapshot exists" means "we are already shuffled" and nothing else.
async fn snapshot_preshuffle_order(pool: &SqlitePool, ids: &[i64]) -> AppResult<()> {
    if read_setting_string(pool, "queue.preshuffle")
        .await?
        .is_some()
    {
        return Ok(());
    }
    let json =
        serde_json::to_string(ids).map_err(|e| AppError::Other(format!("preshuffle json: {e}")))?;
    write_setting_string(pool, "queue.preshuffle", &json).await
}

/// Shuffle whole records instead of individual tracks (#618).
///
/// Only the order of the albums is randomised. Inside each one the
/// tracks are put back into disc and track order, so a record that
/// arrived in the queue scrambled still plays the way it was pressed.
///
/// Two rules the plain track shuffle does not have to answer:
///
/// - **The record you are in continues.** The current track stays at
///   position 0, the rest of its album follows from the next track
///   onward, and the tracks before it come after the last one — a
///   rotation, so nothing is dropped and the album still plays in
///   order from where you are.
/// - **A track with no album is its own record.** Loose files then
///   shuffle exactly as they would have under track shuffle, instead
///   of being welded into one arbitrary block by a shared NULL.
pub async fn shuffle_by_album(pool: &SqlitePool) -> AppResult<()> {
    // `position` decides ties inside a record, so a pair of tracks that
    // both lack a track number keeps the order it already had rather
    // than depending on how the rows came back.
    let rows: Vec<QueuedAlbumTrack> = sqlx::query_as(
        "SELECT q.track_id, q.position, t.album_id, t.disc_number, t.track_number
           FROM queue_item q
           LEFT JOIN track t ON t.id = q.track_id
          ORDER BY q.position",
    )
    .fetch_all(pool)
    .await?;
    if rows.len() < 2 {
        return Ok(());
    }

    let current_index = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0)
        .clamp(0, rows.len() as i64 - 1) as usize;
    let current_id = rows[current_index].0;

    // The order the listener built, so turning shuffle off restores it
    // exactly as it does after a track shuffle.
    snapshot_preshuffle_order(pool, &rows.iter().map(|(id, ..)| *id).collect::<Vec<_>>()).await?;

    let mut groups = album_runs(&rows, current_id);
    // Only the order of the records is random; `album_runs` has
    // already settled everything inside them.
    let tail = &mut groups[1..];
    fisher_yates(tail);

    let ordered: Vec<i64> = groups.into_iter().flatten().collect();
    write_queue_order(pool, &ordered, 0).await
}

/// One queue row, as [`album_runs`] needs it: the track, where it sits
/// now, and what the library knows about its place on a record.
type QueuedAlbumTrack = (i64, i64, Option<i64>, Option<i64>, Option<i64>);

/// Split a queue into records, each in the order it was pressed in,
/// with the record being listened to first and rotated onto the
/// current track.
///
/// Pure so the grouping and the rotation are testable without a
/// database — the same reason [`cursor_after_removal`] is. Everything
/// random about album shuffle is the caller's single `fisher_yates`
/// over the returned tail; nothing here depends on chance, so a
/// failure is reproducible.
///
/// The first run is always the one holding `current_id`, so callers
/// can shuffle `[1..]` and leave the current track at position 0.
fn album_runs(rows: &[QueuedAlbumTrack], current_id: i64) -> Vec<Vec<i64>> {
    // First-appearance order, so the grouping itself is deterministic.
    let mut groups: Vec<Vec<&QueuedAlbumTrack>> = Vec::new();
    let mut index_of: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for row in rows {
        match row.2 {
            Some(album) => match index_of.get(&album) {
                Some(&at) => groups[at].push(row),
                None => {
                    index_of.insert(album, groups.len());
                    groups.push(vec![row]);
                }
            },
            // A track with no album is its own record. Grouping them
            // all under one NULL would weld unrelated loose files into
            // a single block that always plays together.
            None => groups.push(vec![row]),
        }
    }

    for group in &mut groups {
        group.sort_by_key(|(_, position, _, disc, track_no)| {
            // A missing disc or track number sorts after every numbered
            // track rather than before it: an untagged bonus track
            // belongs at the end, not in front of track 1. `position`
            // breaks the remaining ties, so two equally untagged tracks
            // keep the order they already had instead of depending on
            // how the rows came back.
            (
                disc.unwrap_or(i64::MAX),
                track_no.unwrap_or(i64::MAX),
                *position,
            )
        });
    }

    let current_group = groups
        .iter()
        .position(|group| group.iter().any(|(id, ..)| *id == current_id))
        .unwrap_or(0);
    let mut ordered: Vec<Vec<i64>> = Vec::with_capacity(groups.len());
    let mut first: Vec<i64> = groups
        .remove(current_group)
        .into_iter()
        .map(|(id, ..)| *id)
        .collect();
    // Carry on from where the listener is, then wrap to the start of
    // the record. Nothing is dropped — a shuffle is a permutation —
    // and the album still plays in order from the current track.
    if let Some(at) = first.iter().position(|id| *id == current_id) {
        first.rotate_left(at);
    }
    ordered.push(first);
    ordered.extend(
        groups
            .into_iter()
            .map(|group| group.into_iter().map(|(id, ..)| *id).collect()),
    );
    ordered
}

/// Restore the pre-shuffle order from `queue.preshuffle` and re-home
/// the cursor onto the currently-playing track's position in that
/// restored ordering.
pub async fn unshuffle(pool: &SqlitePool) -> AppResult<()> {
    let json = match read_setting_string(pool, "queue.preshuffle").await? {
        Some(s) => s,
        None => return Ok(()),
    };
    let original: Vec<i64> = serde_json::from_str(&json)
        .map_err(|e| AppError::Other(format!("preshuffle parse: {e}")))?;
    if original.is_empty() {
        return Ok(());
    }

    // Find the currently-playing track in the restored order.
    let current = current_track(pool).await?;
    let new_index = match current {
        Some(t) => original.iter().position(|&id| id == t.id).unwrap_or(0),
        None => 0,
    };

    write_queue_order(pool, &original, new_index).await?;
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(pool)
        .await?;
    Ok(())
}

/// Where the cursor lands after `[start, end)` (which held `removed` rows) is
/// deleted from the queue and positions are compacted, so the **same track
/// keeps playing**:
///   - deletions entirely before the cursor shift it down by `removed`;
///   - a deletion that spans the cursor lands it on whatever now occupies
///     `start` (the track that fell into the gap);
///   - deletions after the cursor leave it put.
///
/// The result is clamped into the shrunk queue `[0, new_len)`.
///
/// Pure so the cursor arithmetic is unit-testable without a database — the
/// bug it guards (only clamping, so a deletion before the cursor silently
/// advanced to the next track) isn't visible from the clamp alone.
fn cursor_after_removal(current: i64, start: i64, end: i64, removed: i64, new_len: i64) -> i64 {
    let adjusted = if current >= end {
        current - removed
    } else if current >= start {
        start
    } else {
        current
    };
    adjusted.clamp(0, (new_len - 1).max(0))
}

/// Persist the [`cursor_after_removal`] adjustment inside the open removal
/// transaction. Whenever rows leave the queue the cursor can dangle past the
/// end (and [`advance`] would then have nothing to step from) or, worse,
/// silently point at a different track; this keeps it on the same one.
async fn adjust_current_index_after_removal(
    tx: &mut sqlx::SqliteConnection,
    start: i64,
    end: i64,
    removed: i64,
    new_len: i64,
) -> AppResult<()> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = 'queue.current_index'")
            .fetch_optional(&mut *tx)
            .await?;
    let current = raw.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let next = cursor_after_removal(current, start, end, removed, new_len);
    if next != current {
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "UPDATE profile_setting SET value = ?, updated_at = ?
              WHERE key = 'queue.current_index'",
        )
        .bind(next.to_string())
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

/// Empty the queue and reset the cursor.
///
/// Playback is deliberately left alone — the decoder keeps whatever it
/// already loaded. MPD's `clear` behaves the same way: it empties the
/// queue, and stopping is a separate `stop`.
pub async fn clear(pool: &SqlitePool) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM queue_item")
        .execute(&mut *tx)
        .await?;
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE profile_setting SET value = '0', updated_at = ?
          WHERE key = 'queue.current_index'",
    )
    .bind(now)
    .execute(&mut *tx)
    .await?;
    // The pre-shuffle snapshot describes a queue that no longer exists.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Remove the half-open position range `[start, end)` and compact the
/// positions above it. Returns the number of rows removed.
///
/// `position` carries a UNIQUE constraint and SQLite enforces it per
/// row, so the surviving rows cannot simply be decremented in place —
/// a row moving down onto a slot whose occupant has not moved yet would
/// collide. Same `PARK` detour [`reorder`] uses.
pub async fn remove_range(pool: &SqlitePool, start: i64, end: i64) -> AppResult<u64> {
    const PARK: i64 = 10_000_000;
    let mut tx = pool.begin().await?;

    // Read the length INSIDE the transaction so the clamp, the delete range and
    // `new_len` all derive from the same consistent snapshot the DELETE acts
    // on — a concurrent queue write between a pre-transaction count and the
    // delete would otherwise skew the cursor adjustment.
    let len: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;
    let start = start.clamp(0, len);
    let end = end.clamp(0, len);
    if end <= start {
        return Ok(0);
    }
    let span = end - start;

    let removed = sqlx::query("DELETE FROM queue_item WHERE position >= ? AND position < ?")
        .bind(start)
        .bind(end)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    sqlx::query("UPDATE queue_item SET position = position + ? WHERE position >= ?")
        .bind(PARK)
        .bind(end)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE queue_item SET position = position - ? - ? WHERE position >= ?")
        .bind(PARK)
        .bind(span)
        .bind(PARK)
        .execute(&mut *tx)
        .await?;

    adjust_current_index_after_removal(&mut tx, start, end, removed as i64, len - removed as i64)
        .await?;
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(removed)
}

/// Remove one entry by its `queue_item.id`.
///
/// Keyed on the row id rather than the position because that is the
/// identity MPD's `deleteid` carries, and it stays valid even if the
/// queue was reordered between the client reading it and acting on it.
/// Returns `false` when no such row exists.
pub async fn remove_by_queue_id(pool: &SqlitePool, queue_id: i64) -> AppResult<bool> {
    let position: Option<i64> = sqlx::query_scalar("SELECT position FROM queue_item WHERE id = ?")
        .bind(queue_id)
        .fetch_optional(pool)
        .await?;
    let Some(position) = position else {
        return Ok(false);
    };
    Ok(remove_range(pool, position, position + 1).await? > 0)
}

/// Look up a queue row's position from its id.
pub async fn position_of_queue_id(pool: &SqlitePool, queue_id: i64) -> AppResult<Option<i64>> {
    let position: Option<i64> = sqlx::query_scalar("SELECT position FROM queue_item WHERE id = ?")
        .bind(queue_id)
        .fetch_optional(pool)
        .await?;
    Ok(position)
}

/// Rewrite `queue_item` with the given ordering and update the
/// current index pointer. Runs in a transaction.
async fn write_queue_order(
    pool: &SqlitePool,
    ordered_ids: &[i64],
    new_current: usize,
) -> AppResult<()> {
    // Where each track came from, kept across the reorder. Writing
    // every row back as 'manual' would throw away two things that are
    // read later: the source a play_event is attributed to, and the
    // boundary `fill_queue` uses to tell queued-up "play next" items
    // from the source queue they were dropped into.
    //
    // Keyed by track id with one entry per occurrence, in position
    // order, so a queue holding the same track twice hands each copy
    // back its own source rather than the first one's.
    let mut sources: std::collections::HashMap<
        i64,
        std::collections::VecDeque<(String, Option<i64>)>,
    > = std::collections::HashMap::new();
    //
    // Read *inside* the transaction that rewrites them, so the rows
    // put back describe the queue being replaced rather than one that
    // changed in between.
    let mut tx = pool.begin().await?;
    let existing: Vec<(i64, String, Option<i64>)> =
        sqlx::query_as("SELECT track_id, source_type, source_id FROM queue_item ORDER BY position")
            .fetch_all(&mut *tx)
            .await?;
    for (track_id, source_type, source_id) in existing {
        sources
            .entry(track_id)
            .or_default()
            .push_back((source_type, source_id));
    }

    sqlx::query("DELETE FROM queue_item")
        .execute(&mut *tx)
        .await?;
    let now = Utc::now().timestamp_millis();
    for (pos, track_id) in ordered_ids.iter().enumerate() {
        // 'manual' only for a track the old queue did not hold, which
        // is not something any caller here does today.
        let (source_type, source_id) = sources
            .get_mut(track_id)
            .and_then(|queued| queued.pop_front())
            .unwrap_or_else(|| ("manual".to_string(), None));
        sqlx::query(
            "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(track_id)
        .bind(pos as i64)
        .bind(source_type)
        .bind(source_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = 'queue.current_index'",
    )
    .bind((new_current as i64).to_string())
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// In-place Fisher–Yates using [`fastrand`] for simplicity.
///
/// We deliberately don't pull in a full RNG crate here — shuffling the
/// queue is not a security-critical operation and `fastrand` is ~20
/// lines of code and already a transitive dep via some other crate.
fn fisher_yates<T>(slice: &mut [T]) {
    // Simple linear congruential RNG seeded from the clock. Good
    // enough for shuffling a music queue.
    let mut seed: u64 = Utc::now().timestamp_millis() as u64;
    for i in (1..slice.len()).rev() {
        // xorshift step
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let j = (seed % (i as u64 + 1)) as usize;
        slice.swap(i, j);
    }
}

// Re-export the player state snapshot type here so the analytics task
// can keep its imports minimal. Not a real runtime dependency.
#[allow(dead_code)]
fn _type_check() -> Option<PlayerStateSnapshot> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Album shuffle (#618)
    // ------------------------------------------------------------------

    /// `(track_id, position, album_id, disc, track_no)` — the shape
    /// the queue query hands [`album_runs`].
    fn queued(
        id: i64,
        position: i64,
        album: Option<i64>,
        disc: Option<i64>,
        track_no: Option<i64>,
    ) -> QueuedAlbumTrack {
        (id, position, album, disc, track_no)
    }

    /// Two records interleaved in the queue come back as two blocks,
    /// each in its own disc/track order rather than the order the
    /// queue happened to hold.
    #[test]
    fn album_runs_group_records_and_restore_their_order() {
        let rows = vec![
            queued(30, 0, Some(1), Some(1), Some(3)),
            queued(20, 1, Some(2), Some(1), Some(2)),
            queued(10, 2, Some(1), Some(1), Some(1)),
            queued(21, 3, Some(2), Some(1), Some(1)),
            queued(11, 4, Some(1), Some(1), Some(2)),
        ];
        let runs = album_runs(&rows, 10);
        assert_eq!(runs.len(), 2, "two albums, two runs");
        assert_eq!(runs[0], vec![10, 11, 30], "album 1 in track order");
        assert!(
            runs[1..].iter().any(|r| *r == vec![21, 20]),
            "album 2 in track order, got {runs:?}"
        );
    }

    /// The record being listened to comes first, and carries on from
    /// the current track rather than restarting: a rotation, so the
    /// tracks already behind you come back round at the end instead of
    /// vanishing from the queue.
    #[test]
    fn the_current_record_continues_from_where_it_is() {
        let rows = vec![
            queued(1, 0, Some(7), Some(1), Some(1)),
            queued(2, 1, Some(7), Some(1), Some(2)),
            queued(3, 2, Some(7), Some(1), Some(3)),
            queued(4, 3, Some(7), Some(1), Some(4)),
            queued(99, 4, Some(8), Some(1), Some(1)),
        ];
        let runs = album_runs(&rows, 3);
        assert_eq!(runs[0], vec![3, 4, 1, 2]);
        assert_eq!(runs[0][0], 3, "the current track keeps position 0");
    }

    /// A shuffle is a permutation. Whatever the grouping does, every
    /// track that went in comes back out exactly once — the bug that
    /// would quietly shorten someone's queue.
    #[test]
    fn every_track_survives_the_grouping_exactly_once() {
        let rows = vec![
            queued(1, 0, Some(7), Some(1), Some(2)),
            queued(2, 1, None, None, None),
            queued(3, 2, Some(7), Some(2), Some(1)),
            queued(4, 3, Some(8), None, None),
            queued(5, 4, None, None, None),
            queued(6, 5, Some(7), Some(1), Some(1)),
        ];
        let mut seen: Vec<i64> = album_runs(&rows, 3).into_iter().flatten().collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2, 3, 4, 5, 6]);
    }

    /// A track with no album is its own record. Grouping every NULL
    /// together would weld unrelated loose files into one block that
    /// always plays in the same order — the opposite of shuffling.
    #[test]
    fn tracks_with_no_album_are_each_their_own_record() {
        let rows = vec![
            queued(1, 0, None, None, None),
            queued(2, 1, None, None, None),
            queued(3, 2, None, None, None),
        ];
        let runs = album_runs(&rows, 1);
        assert_eq!(runs.len(), 3, "three loose files, three runs");
        assert!(runs.iter().all(|r| r.len() == 1));
    }

    /// Disc order beats track order, and an untagged track sorts after
    /// every numbered one — a bonus track with no number belongs at
    /// the end of the record, not in front of track 1.
    #[test]
    fn discs_come_in_order_and_untagged_tracks_go_last() {
        let rows = vec![
            queued(40, 0, Some(1), None, None),
            queued(21, 1, Some(1), Some(2), Some(1)),
            queued(12, 2, Some(1), Some(1), Some(2)),
            queued(11, 3, Some(1), Some(1), Some(1)),
            queued(41, 4, Some(1), None, None),
        ];
        let runs = album_runs(&rows, 11);
        assert_eq!(runs.len(), 1);
        // Rotated onto the current track, which is already first here.
        assert_eq!(runs[0], vec![11, 12, 21, 40, 41]);
    }

    /// Two untagged tracks keep the order the queue already had, so the
    /// result does not depend on how the rows came back.
    #[test]
    fn ties_fall_back_to_the_position_the_queue_already_had() {
        let rows = vec![
            queued(50, 3, Some(1), None, None),
            queued(51, 1, Some(1), None, None),
            queued(52, 2, Some(1), None, None),
        ];
        let runs = album_runs(&rows, 51);
        assert_eq!(runs[0], vec![51, 52, 50]);
    }

    /// The current track not being in the queue at all should not
    /// panic or drop a record — the first run is simply whichever came
    /// first.
    #[test]
    fn an_absent_current_track_still_yields_every_run() {
        let rows = vec![
            queued(1, 0, Some(7), Some(1), Some(1)),
            queued(2, 1, Some(8), Some(1), Some(1)),
        ];
        let runs = album_runs(&rows, 999);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], vec![1]);
    }

    /// The mode is persisted as a string other surfaces read back.
    #[test]
    fn shuffle_mode_round_trips_through_string() {
        for mode in [ShuffleMode::Off, ShuffleMode::Tracks, ShuffleMode::Albums] {
            assert_eq!(ShuffleMode::from_str(mode.as_str()), mode);
        }
        for junk in ["", "album", "ALBUMS", "true"] {
            assert_eq!(ShuffleMode::from_str(junk), ShuffleMode::Off);
        }
        assert!(!ShuffleMode::Off.is_on());
        assert!(ShuffleMode::Tracks.is_on());
        assert!(ShuffleMode::Albums.is_on());
    }

    #[test]
    fn repeat_mode_round_trips_through_string() {
        for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
            assert_eq!(RepeatMode::from_str(mode.as_str()), mode);
        }
    }

    #[test]
    fn repeat_mode_unknown_string_falls_back_to_off() {
        // The DB stores user-facing strings; an old / corrupted value
        // must not panic. "Off" is the conservative default.
        assert_eq!(RepeatMode::from_str("garbage"), RepeatMode::Off);
        assert_eq!(RepeatMode::from_str(""), RepeatMode::Off);
    }

    #[test]
    fn removing_before_the_cursor_keeps_the_same_track() {
        // Queue [A,B,C,D] len 4, cursor 2 (C is playing).
        // Delete [0,1) (A) → C is now at position 1, so the cursor follows.
        assert_eq!(cursor_after_removal(2, 0, 1, 1, 3), 1);
        // Delete [0,2) (A,B) → C is now at position 0.
        assert_eq!(cursor_after_removal(2, 0, 2, 2, 2), 0);
    }

    #[test]
    fn removing_at_or_after_the_cursor_behaves() {
        // Delete after the cursor → the cursor doesn't move.
        assert_eq!(cursor_after_removal(1, 2, 4, 2, 2), 1);
        // Delete a span that includes the playing track → land on `start`,
        // whatever fell into the gap.
        assert_eq!(cursor_after_removal(2, 1, 3, 2, 2), 1);
        // Delete the whole queue → clamp to 0.
        assert_eq!(cursor_after_removal(0, 0, 4, 4, 0), 0);
    }
}

#[cfg(test)]
mod step_tests {
    use super::{stepped_index, Direction, RepeatMode};

    #[test]
    fn next_stops_at_the_end_with_repeat_off() {
        assert_eq!(
            stepped_index(3, 2, Direction::Next, RepeatMode::Off),
            None,
            "the step runs off the end, so there is nothing to commit"
        );
        assert_eq!(
            stepped_index(3, 1, Direction::Next, RepeatMode::Off),
            Some(2)
        );
    }

    #[test]
    fn repeat_all_wraps_both_ways() {
        assert_eq!(
            stepped_index(3, 2, Direction::Next, RepeatMode::All),
            Some(0)
        );
        assert_eq!(
            stepped_index(3, 0, Direction::Previous, RepeatMode::All),
            Some(2)
        );
    }

    #[test]
    fn repeat_one_stays_put_whichever_way_you_step() {
        assert_eq!(
            stepped_index(3, 1, Direction::Next, RepeatMode::One),
            Some(1)
        );
        assert_eq!(
            stepped_index(3, 1, Direction::Previous, RepeatMode::One),
            Some(1)
        );
    }

    #[test]
    fn previous_clamps_at_the_start_with_repeat_off() {
        assert_eq!(
            stepped_index(3, 0, Direction::Previous, RepeatMode::Off),
            Some(0)
        );
    }

    #[test]
    fn a_cursor_past_the_end_steps_from_inside_the_queue() {
        // `queue.current_index` can outlive the rows it pointed at — a
        // queue that shrank under it. Stepping from there would otherwise
        // read a row that no longer exists.
        assert_eq!(
            stepped_index(3, 99, Direction::Next, RepeatMode::All),
            Some(0)
        );
        assert_eq!(
            stepped_index(3, 99, Direction::Previous, RepeatMode::Off),
            Some(1)
        );
    }

    #[test]
    fn an_empty_queue_has_nowhere_to_step() {
        assert_eq!(stepped_index(0, 0, Direction::Next, RepeatMode::All), None);
    }
}

#[cfg(test)]
mod fill_queue_tests {
    use super::{append_to_user_queue, fill_queue, insert_after_current, read_album_ordered};
    use sqlx::SqlitePool;

    /// The repo's own profile migrations, with `foreign_keys` on —
    /// `fill_queue` writes rows that reference `track`, and the setting
    /// it stores is written by a query nothing checks at compile time.
    async fn migrated_pool() -> SqlitePool {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let options = SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(
            "INSERT INTO library (id, name, created_at, updated_at)
                  VALUES (1, 'l', 0, 0);
             INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at)
                  VALUES (1, 1, '/l/a.flac', 'h1', 1, 0, 'A', 1000, 0),
                         (2, 1, '/l/b.flac', 'h2', 1, 0, 'B', 1000, 0);",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    /// Clearing it matters as much as setting it (#647): a flag left
    /// raised would hand album gain to the next ordinary playlist, and
    /// the whole reason this lives in `fill_queue` is that every queue
    /// replacement passes through here.
    #[tokio::test]
    async fn the_queue_records_whether_it_was_built_from_records() {
        let pool = migrated_pool().await;

        // Nothing stored yet is not "album-ordered".
        assert!(!read_album_ordered(&pool).await);

        fill_queue(&pool, "radio", None, &[1, 2], 0, true)
            .await
            .unwrap();
        assert!(read_album_ordered(&pool).await);

        fill_queue(&pool, "playlist", Some(7), &[1], 0, false)
            .await
            .unwrap();
        assert!(
            !read_album_ordered(&pool).await,
            "the next queue is not a session of records because the last one was"
        );
    }

    /// `fill_queue` is not the only path that builds a queue where
    /// there was none, which is exactly how a flag survives the session
    /// it described: both "Play next" and "Add to queue" fill an empty
    /// queue themselves, the second one inline.
    #[tokio::test]
    async fn the_hand_built_queues_clear_it_too() {
        for (label, replaced) in [("play next", true), ("add to queue", false)] {
            let pool = migrated_pool().await;
            fill_queue(&pool, "radio", None, &[1, 2], 0, true)
                .await
                .unwrap();
            // Emptied the way a removal would leave it.
            sqlx::query("DELETE FROM queue_item")
                .execute(&pool)
                .await
                .unwrap();

            let was_replaced = if replaced {
                insert_after_current(&pool, &[1], "manual", None)
                    .await
                    .unwrap()
            } else {
                append_to_user_queue(&pool, &[1], None).await.unwrap()
            };

            assert!(was_replaced, "{label} filled an empty queue");
            assert!(
                !read_album_ordered(&pool).await,
                "{label} built a queue by hand, so the old session's flag is gone"
            );
        }
    }

    /// And neither touches it when there is a queue to insert into:
    /// adding a track to a record playing through does not stop it
    /// being one.
    #[tokio::test]
    async fn inserting_into_a_live_queue_leaves_the_session_alone() {
        let pool = migrated_pool().await;
        fill_queue(&pool, "radio", None, &[1, 2], 0, true)
            .await
            .unwrap();

        assert!(!insert_after_current(&pool, &[2], "manual", None)
            .await
            .unwrap());
        assert!(!append_to_user_queue(&pool, &[2], None).await.unwrap());
        assert!(read_album_ordered(&pool).await);
    }
}
