//! An episode playing: its session, where its audio comes from, its position.
//!
//! An episode is not a track. It plays under a negative id through a
//! [`DetachedSession`], the queue in memory the remote queue and the files played
//! without importing them use, so the ordering rules of #622 hold for it without a
//! copy of them. Nothing here writes a `play_event`, and the decoder sends nothing
//! that would: a negative id credits nobody.
//!
//! ## Where the audio comes from
//!
//! A downloaded file plays from disk, with no request, whatever the switch and
//! offline mode say. Without one, the switch off or offline mode on refuses here,
//! before anything changes, and the decoder asks again before every request of a
//! stream ([`super::stream`]).
//!
//! ## The position
//!
//! Written from the async side, never from the audio path. The decoder reports
//! where an episode was left ([`record_left`]), which covers a stop, another track
//! and the exit, whose wait on the analytics channel covers this too. A pause, a
//! seek and every [`SAVE_EVERY`] of playing are seen by [`watch`], which reads the
//! engine's atomics once a second while a session is active and sleeps otherwise.
//!
//! ## Speed
//!
//! The speed travels with the load and the decoder holds it for as long as that
//! load decodes, so everything that plays afterwards, by whatever path, gets the
//! global speed back without knowing an episode was there. `audio.playback_speed`
//! is never written: a slider moved during an episode writes the channel's.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Listener, Manager};
use tokio::sync::Notify;
use url::Url;
use waveflow_core::artwork::metadata::existing_path;
use waveflow_core::podcast::address::parse_address;
use waveflow_core::podcast::FetchFailure;

use super::download::stored_file;
use super::fetch::{Fetcher, Origin};
use super::refresh::PodcastError;
use super::schedule::emit_updated;
use super::store::{self, PlayableEpisode};
use super::stream::{self, EnclosureSource, Gate, StreamRefusal};
use crate::audio::engine::{AudioCmd, LastLoadSource, LoadIntent};
use crate::audio::state::PlayerState;
use crate::audio::AudioEngine;
use crate::commands::profile::PROFILE_SETTING_CHANGED;
use crate::error::{AppError, AppResult};
use crate::queue::{Direction, RepeatMode};
use crate::remote_playback::{DetachedQueue, DetachedSession};
use crate::state::AppState;

/// How often a position is written while an episode plays, beside a pause and a
/// seek.
pub(crate) const SAVE_EVERY: Duration = Duration::from_secs(15);

/// How often the engine is looked at while a session is active.
const WATCH_EVERY: Duration = Duration::from_secs(1);

/// The source type an episode's stream carries. It is what keeps ReplayGain off
/// it, in the decoder's own terms: `audio::decoder::track::apply_replay_gain`.
pub(crate) const SOURCE_TYPE: &str = "podcast";

/// What the decoder needs to play one episode. Its `Debug` holds no address: this
/// reaches every logger that prints a command.
#[derive(Clone, PartialEq)]
pub struct EpisodeLoad {
    pub episode_id: i64,
    pub channel_id: i64,
    /// Whose database the position goes into, whatever is active by then.
    pub profile_id: i64,
    /// The negative sentinel it plays under.
    pub track_id: i64,
    pub start_ms: u64,
    /// `0` when the feed does not say: the decoder then asks the container.
    pub duration_ms: u64,
    pub title: String,
    pub channel: String,
    pub artwork_path: Option<String>,
    /// Held for as long as this load decodes.
    pub speed: f32,
    pub source: EpisodeSource,
}

impl std::fmt::Debug for EpisodeLoad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "EpisodeLoad {{ episode_id: {}, track_id: {}, start_ms: {} }}",
            self.episode_id, self.track_id, self.start_ms
        )
    }
}

/// Where an episode's bytes are.
#[derive(Clone, PartialEq)]
pub enum EpisodeSource {
    File(PathBuf),
    Stream {
        enclosure: Url,
        origin: Origin,
        /// The type the feed declared, for the probe when the host says nothing.
        declared_type: Option<String>,
    },
}

/// One episode of the queue. Its row is read again when it plays.
#[derive(Debug, Clone)]
pub struct EpisodeEntry {
    pub episode_id: i64,
    pub channel_id: i64,
    pub profile_id: i64,
    pub title: String,
    pub channel: String,
    pub duration_ms: Option<i64>,
}

/// The session of a channel's episodes.
pub type PodcastPlayback = DetachedSession<EpisodeEntry>;

/// Wakes [`watch`] when a session starts.
static SESSION_STARTED: Notify = Notify::const_new();

/// What the player shows while an episode's channel has no picture on disk, or
/// the feed names no title.
fn display_title(title: &str, channel: &str) -> String {
    if title.trim().is_empty() {
        channel.to_owned()
    } else {
        title.to_owned()
    }
}

/// Where this episode's bytes come from, or why it cannot play.
///
/// A file on disk needs nobody, so it comes first and the two switches do not
/// reach it. Everything else is a request, and refused when one may not be made.
pub(crate) fn resolve_source(
    episode: &PlayableEpisode,
    downloads: &Path,
    enabled: bool,
    offline: bool,
) -> Result<EpisodeSource, PodcastError> {
    if let Some(path) = episode
        .download_file
        .as_deref()
        .and_then(|name| stored_file(downloads, episode.id, name))
    {
        return Ok(EpisodeSource::File(path));
    }
    if !enabled {
        return Err(PodcastError::Disabled);
    }
    if offline {
        return Err(PodcastError::Offline);
    }
    let raw = episode
        .enclosure_url
        .as_deref()
        .ok_or(PodcastError::NoSource)?;
    let enclosure = parse_address(raw).ok_or(PodcastError::Failed(FetchFailure::AddressRefused))?;
    // A channel always has an address when an episode does, but a row is what a
    // database says: with nothing to go by, the stricter of the two rules.
    let origin = episode
        .feed_url
        .as_deref()
        .and_then(parse_address)
        .map_or_else(Origin::public, |typed| Origin::of(&typed));
    Ok(EpisodeSource::Stream {
        enclosure,
        origin,
        declared_type: episode.enclosure_type.clone(),
    })
}

/// Where an episode opens: from its position, or from the top once it counts as
/// played (decision 7).
pub(crate) fn start_position(episode: &PlayableEpisode) -> u64 {
    if episode.played_at.is_some() {
        return 0;
    }
    crate::queue::resume_position(
        episode.position_ms.max(0) as u64,
        episode.duration_ms.unwrap_or(0).max(0) as u64,
    )
}

/// The speed an episode of this channel plays at. `NULL` is the normal one, not
/// the global: a speed chosen for music does not reach a voice either.
pub(crate) fn channel_speed(stored: Option<f64>) -> f32 {
    stored
        .filter(|speed| speed.is_finite())
        .map_or(1.0, |speed| (speed as f32).clamp(0.5, 2.0))
}

/// The queue a channel's episodes form, newest first as the page lists them, and
/// where `current` is in it. With no request allowed, only what is on disk.
fn queue_of(
    rows: Vec<store::QueueRow>,
    profile_id: i64,
    channel: &str,
    current: i64,
    can_stream: bool,
) -> (Vec<EpisodeEntry>, usize) {
    let entries: Vec<EpisodeEntry> = rows
        .into_iter()
        .filter(|row| row.id == current || row.downloaded || (can_stream && row.has_enclosure))
        .map(|row| EpisodeEntry {
            episode_id: row.id,
            channel_id: row.channel_id,
            profile_id,
            title: display_title(&row.title, channel),
            channel: channel.to_owned(),
            duration_ms: row.duration_ms,
        })
        .collect();
    let index = entries
        .iter()
        .position(|entry| entry.episode_id == current)
        .unwrap_or(0);
    (entries, index)
}

/// Starts `episode_id`, with its channel as the queue. A refusal changes nothing:
/// what plays keeps playing.
pub(crate) async fn play_episode(app: &AppHandle, episode_id: i64) -> AppResult<()> {
    let engine = app.state::<Arc<AudioEngine>>();
    // Claimed before anything is read (#622): a track picked while this prepares
    // is the newer choice.
    let intent = engine.next_load_intent();
    let state = app.state::<AppState>();
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let episode = store::playable(&pool, episode_id)
        .await?
        .ok_or(PodcastError::EpisodeGone)?;
    let enabled = store::is_enabled(&pool).await?;
    let offline = crate::offline::is_offline();
    let downloads = state.paths.profile_podcast_download_dir(profile_id);
    let source = resolve_source(&episode, &downloads, enabled, offline)?;
    let rows = store::queue_rows(&pool, episode.channel_id).await?;
    let channel = episode.channel_title.clone();
    let (entries, index) = queue_of(rows, profile_id, &channel, episode_id, enabled && !offline);

    // Held across the send, as for files: everything from here on is local.
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        tracing::debug!("episode superseded before it started; not installing it");
        return Ok(());
    }
    // One detached queue at a time.
    state.remote_playback.clear();
    state.external_playback.clear();
    let revision = state.podcast_playback.set(DetachedQueue { entries, index });
    SESSION_STARTED.notify_one();
    let played = send_load(app, &pool, profile_id, episode, source, intent).await;
    if played.is_err() {
        let _ = engine.send(AudioCmd::Stop);
        state.podcast_playback.clear_if(revision);
    } else if engine.load_intent_superseded(intent) {
        state.podcast_playback.clear_if(revision);
    }
    played
}

/// Reads the row under the cursor again and plays it. For a step, a jump and Play
/// from a stopped player: what was playable when the queue was made may not be now.
async fn play_current(app: &AppHandle, intent: LoadIntent) -> AppResult<()> {
    let state = app.state::<AppState>();
    let Some(entry) = state.podcast_playback.current() else {
        return Ok(());
    };
    let pool = state
        .require_profile_pool_for(Some(entry.profile_id))
        .await
        .map_err(|_| PodcastError::ProfileChanged)?;
    let episode = store::playable(&pool, entry.episode_id)
        .await?
        .ok_or(PodcastError::EpisodeGone)?;
    let enabled = store::is_enabled(&pool).await?;
    let source = resolve_source(
        &episode,
        &state.paths.profile_podcast_download_dir(entry.profile_id),
        enabled,
        crate::offline::is_offline(),
    )?;
    send_load(app, &pool, entry.profile_id, episode, source, intent).await
}

/// Hands the episode to the decoder and tells the surfaces outside the window. The
/// bar and the mini-player hear it from the decoder, once it has it.
async fn send_load(
    app: &AppHandle,
    pool: &sqlx::SqlitePool,
    profile_id: i64,
    episode: PlayableEpisode,
    source: EpisodeSource,
    intent: LoadIntent,
) -> AppResult<()> {
    let start_ms = start_position(&episode);
    if let Err(err) = store::note_opened(pool, episode.id).await {
        // A column nobody needs to hear this episode.
        tracing::warn!(%err, "podcast: opened time not saved");
    }
    let state = app.state::<AppState>();
    let artwork_path = episode
        .artwork_hash
        .as_deref()
        .and_then(|hash| existing_path(&state.paths.metadata_artwork_dir, hash));
    let load = EpisodeLoad {
        episode_id: episode.id,
        channel_id: episode.channel_id,
        profile_id,
        track_id: crate::commands::player::next_radio_track_id(),
        start_ms,
        duration_ms: episode.duration_ms.unwrap_or(0).max(0) as u64,
        title: display_title(&episode.title, &episode.channel_title),
        channel: episode.channel_title,
        artwork_path,
        speed: channel_speed(episode.playback_speed),
        source,
    };
    announce(app, &load);
    app.state::<Arc<AudioEngine>>()
        .send(AudioCmd::LoadEpisodeAndPlay {
            intent,
            episode: Box::new(load),
        })
}

/// The OS media overlay and the tray say what plays. Discord's card is cleared
/// rather than updated: it is not a track, and what somebody listens to in a
/// podcast is not announced for them.
fn announce(app: &AppHandle, load: &EpisodeLoad) {
    if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
        controls.update_metadata(
            load.title.clone(),
            Some(load.channel.clone()),
            None,
            load.artwork_path.clone(),
            load.duration_ms as i64,
        );
        controls.update_playback(PlayerState::Playing, load.start_ms);
    }
    if let Some(presence) = app.try_state::<crate::discord_presence::DiscordPresenceHandle>() {
        presence.clear();
    }
    if let Some(tray) = app.tray_by_id("waveflow") {
        let _ = tray.set_tooltip(Some(format!("{} — {}", load.title, load.channel)));
    }
}

/// How a step came about, which decides whether it passes over what was heard.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Step {
    /// A person asked: they go where they asked.
    Asked(Direction),
    /// The episode ended. The next one that has not been heard follows, or a
    /// channel's back catalogue would replay itself through what they already know.
    Ended,
}

/// Steps the cursor and plays what it lands on. `Ok(false)` when the queue ran off
/// its end: the session is cleared and the engine stopped. `intent` is the caller's,
/// claimed where the step started (a key press, the end of an episode).
pub(crate) async fn advance(app: &AppHandle, step: Step, intent: LoadIntent) -> AppResult<bool> {
    let state = app.state::<AppState>();
    let repeat = match state.require_profile_pool().await {
        Ok(pool) => crate::queue::read_repeat_mode(&pool).await,
        Err(_) => RepeatMode::Off,
    };
    let engine = app.state::<Arc<AudioEngine>>();
    // A step overtaken by a newer pick moves nothing (#632).
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        return Ok(false);
    }
    let direction = match step {
        Step::Asked(direction) => direction,
        Step::Ended => Direction::Next,
    };
    let mut landed = state.podcast_playback.step(direction, repeat);
    if matches!(step, Step::Ended) && repeat != RepeatMode::One {
        let hops = state
            .podcast_playback
            .snapshot()
            .map_or(0, |(entries, _)| entries.len());
        for _ in 0..hops {
            let Some(entry) = landed.as_ref() else {
                break;
            };
            let Ok(pool) = state.require_profile_pool_for(Some(entry.profile_id)).await else {
                break;
            };
            if !store::nothing_to_hear(&pool, entry.episode_id)
                .await
                .unwrap_or(false)
            {
                break;
            }
            landed = state.podcast_playback.step(direction, repeat);
        }
    }
    if landed.is_none() {
        let _ = engine.send(AudioCmd::Stop);
        return Ok(false);
    }
    if let Err(err) = play_current(app, intent).await {
        let _ = engine.send(AudioCmd::Stop);
        state.podcast_playback.clear();
        return Err(err);
    }
    Ok(true)
}

/// Moves the cursor to `index` and plays it: the queue panel's click.
pub(crate) async fn jump_to(app: &AppHandle, index: usize) -> AppResult<()> {
    let engine = app.state::<Arc<AudioEngine>>();
    let intent = engine.next_load_intent();
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        return Ok(());
    }
    let state = app.state::<AppState>();
    if state.podcast_playback.seek_to(index).is_none() {
        return Ok(());
    }
    play_current(app, intent).await.inspect_err(|_| {
        let _ = engine.send(AudioCmd::Stop);
        state.podcast_playback.clear();
    })
}

/// Play from a stopped player while a session is still there: the episode under its
/// cursor, from its position. `false` with no session, for the caller to go on with
/// what it would otherwise do. `intent` is the caller's (#622).
pub(crate) async fn resume(app: &AppHandle, intent: LoadIntent) -> AppResult<bool> {
    let state = app.state::<AppState>();
    if !state.podcast_playback.is_active() {
        return Ok(false);
    }
    let engine = app.state::<Arc<AudioEngine>>();
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        return Ok(true);
    }
    play_current(app, intent).await?;
    Ok(true)
}

/// Whether a position counts as heard (decision 7): past 95 %, or within the last
/// 30 seconds. An unknown length says nothing.
pub(crate) fn counts_as_played(position_ms: u64, duration_ms: u64) -> bool {
    if duration_ms == 0 {
        return false;
    }
    position_ms.saturating_mul(100) >= duration_ms.saturating_mul(95)
        || position_ms.saturating_add(30_000) >= duration_ms
}

/// Where an episode stands, written with the rule above. `finished` is an episode
/// that ran to its end, whatever its declared length said.
///
/// Parks behind a scan like any background writer, which a stopped player has time
/// for and a playing one does not need: the next write says the same thing later.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_position(
    app: &AppHandle,
    profile_id: i64,
    episode_id: i64,
    channel_id: i64,
    position_ms: u64,
    duration_ms: u64,
    finished: bool,
    announce_it: bool,
) {
    let state = app.state::<AppState>();
    // That profile's, or nobody's: this may land after a switch, and the id means
    // another episode there.
    let Ok(pool) = state.require_profile_pool_for(Some(profile_id)).await else {
        tracing::debug!("podcast: position dropped, its profile is gone");
        return;
    };
    let played = finished || counts_as_played(position_ms, duration_ms);
    match super::refresh::with_busy_retry(|| {
        store::save_position(&pool, episode_id, position_ms, played)
    })
    .await
    {
        Ok(flipped) => {
            if flipped || announce_it {
                emit_updated(app, Some(channel_id));
            }
        }
        Err(err) => tracing::warn!(%err, "podcast: position not saved"),
    }
}

/// The decoder is done with an episode: stopped, replaced, ended or failed.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_left(
    app: &AppHandle,
    profile_id: i64,
    episode_id: i64,
    channel_id: i64,
    position_ms: u64,
    duration_ms: u64,
    finished: bool,
) {
    record_position(
        app,
        profile_id,
        episode_id,
        channel_id,
        position_ms,
        duration_ms,
        finished,
        true,
    )
    .await;
}

/// Why a position is due. A pause and a seek are worth telling the page about, and
/// the ticks in between are not: it would read the list again every 15 seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveReason {
    Paused,
    Seeked,
    Tick,
}

/// Decides, from what the engine shows, when a position is due. Pure, so the rule
/// can be tested without one.
#[derive(Debug, Default)]
pub(crate) struct SaveClock {
    episode_id: Option<i64>,
    last_saved: Option<Instant>,
    was_paused: bool,
    seek_generation: u64,
}

impl SaveClock {
    pub(crate) fn due(
        &mut self,
        episode_id: i64,
        state: PlayerState,
        seek_generation: u64,
        now: Instant,
    ) -> Option<SaveReason> {
        if self.episode_id != Some(episode_id) {
            // An episode just opened is where its row says. The seek of its own
            // resume happened before this looked.
            *self = Self {
                episode_id: Some(episode_id),
                last_saved: Some(now),
                was_paused: state == PlayerState::Paused,
                seek_generation,
            };
            return None;
        }
        let paused = state == PlayerState::Paused;
        if !paused && state != PlayerState::Playing {
            // Loading, ended, stopped: the position is not this episode's to read,
            // and the decoder reports where it was left.
            return None;
        }
        let reason = if paused && !self.was_paused {
            Some(SaveReason::Paused)
        } else if seek_generation != self.seek_generation {
            Some(SaveReason::Seeked)
        } else if !paused
            && self
                .last_saved
                .map_or(true, |saved| now.duration_since(saved) >= SAVE_EVERY)
        {
            Some(SaveReason::Tick)
        } else {
            None
        };
        self.was_paused = paused;
        self.seek_generation = seek_generation;
        if reason.is_some() {
            self.last_saved = Some(now);
        }
        reason
    }
}

/// Saves the position of the episode playing. Sleeps while no session is active.
async fn watch(app: AppHandle) {
    let mut clock = SaveClock::default();
    loop {
        let entry = app.state::<AppState>().podcast_playback.current();
        let Some(entry) = entry else {
            clock = SaveClock::default();
            SESSION_STARTED.notified().await;
            continue;
        };
        tokio::time::sleep(WATCH_EVERY).await;
        let engine = app.state::<Arc<AudioEngine>>();
        let shared = engine.shared();
        // Read before the checks below, so a load landing in between cannot have
        // this be another episode's clock.
        let position_ms = shared.current_position_ms();
        let Some(metadata) = crate::audio::events::last_radio_metadata().filter(|metadata| {
            metadata.podcast_episode_id == Some(entry.episode_id)
                && metadata.track_id
                    == shared
                        .current_track_id
                        .load(std::sync::atomic::Ordering::Acquire)
        }) else {
            continue;
        };
        let reason = clock.due(
            entry.episode_id,
            shared.state(),
            shared
                .seek_generation
                .load(std::sync::atomic::Ordering::Acquire),
            Instant::now(),
        );
        let Some(reason) = reason else {
            continue;
        };
        record_position(
            &app,
            entry.profile_id,
            entry.episode_id,
            entry.channel_id,
            position_ms,
            metadata.duration_ms.unwrap_or(0).max(0) as u64,
            false,
            reason != SaveReason::Tick,
        )
        .await;
    }
}

/// Starts [`watch`], and the listener that stops a stream when the switch goes off.
/// Costs nothing until an episode plays.
pub(crate) fn spawn(app: AppHandle) {
    let listening = app.clone();
    app.listen(PROFILE_SETTING_CHANGED, move |event| {
        let written = serde_json::from_str::<serde_json::Value>(event.payload())
            .ok()
            .and_then(|payload| payload.get("key")?.as_str().map(str::to_owned));
        if written.as_deref() == Some(store::ENABLED_KEY) {
            let app = listening.clone();
            tauri::async_runtime::spawn(async move { stop_stream_if_switched_off(&app).await });
        }
    });
    tauri::async_runtime::spawn(watch(app));
}

/// The switch went off: "turning podcasts off stops it for good", and a stream is
/// traffic. A downloaded episode is none, and plays on.
async fn stop_stream_if_switched_off(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(entry) = state.podcast_playback.current() else {
        return;
    };
    let Ok(pool) = state.require_profile_pool_for(Some(entry.profile_id)).await else {
        return;
    };
    if store::is_enabled(&pool).await.unwrap_or(true) {
        return;
    }
    let engine = app.state::<Arc<AudioEngine>>();
    let streaming = engine.shared().last_load().is_some_and(|load| {
        matches!(
            load.source,
            LastLoadSource::Episode(ref episode)
                if matches!(episode.source, EpisodeSource::Stream { .. })
        )
    });
    if streaming {
        let _ = engine.send(AudioCmd::Stop);
        state.podcast_playback.clear();
    }
}

/// Opens an episode's stream for the decoder. Blocking: for its thread alone.
pub(crate) fn open_stream(
    app: &AppHandle,
    profile_id: i64,
    enclosure: &Url,
    origin: Origin,
) -> Result<EnclosureSource, StreamRefusal> {
    let fetcher = Fetcher::new().map_err(StreamRefusal::from)?;
    let gate_app = app.clone();
    let gate: Gate = Arc::new(move || {
        let app = gate_app.clone();
        Box::pin(async move { may_stream(&app, profile_id).await })
    });
    stream::EnclosureSource::open(
        tauri::async_runtime::handle().inner().clone(),
        fetcher,
        gate,
        enclosure.clone(),
        origin,
    )
}

/// Asked before every request of a stream.
async fn may_stream(app: &AppHandle, profile_id: i64) -> Result<(), StreamRefusal> {
    if crate::offline::is_offline() {
        return Err(StreamRefusal::Offline);
    }
    let state = app.state::<AppState>();
    let pool = state
        .require_profile_pool_for(Some(profile_id))
        .await
        .map_err(|_| StreamRefusal::Released)?;
    match store::is_enabled(&pool).await {
        Ok(true) => Ok(()),
        // The answer to whether this may run, not a guess: a database that will not
        // say does not get to send traffic.
        Ok(false) | Err(_) => Err(StreamRefusal::Disabled),
    }
}

/// The episodes of the session, for the queue panel, in the shape of a remote queue
/// row so the same view renders either.
#[derive(Debug, Serialize)]
pub struct PodcastQueueEntry {
    /// The episode's id.
    pub id: String,
    pub title: String,
    /// The channel.
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub artwork_hash: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PodcastQueuePayload {
    pub entries: Vec<PodcastQueueEntry>,
    pub index: usize,
}

pub(crate) fn queue_payload(state: &AppState) -> Option<PodcastQueuePayload> {
    let (entries, index) = state.podcast_playback.snapshot()?;
    Some(PodcastQueuePayload {
        entries: entries
            .into_iter()
            .map(|entry| PodcastQueueEntry {
                id: entry.episode_id.to_string(),
                title: entry.title,
                artist: Some(entry.channel),
                artist_id: None,
                artwork_hash: None,
                duration_ms: entry.duration_ms,
            })
            .collect(),
        index,
    })
}

/// A speed chosen for a channel: stored, and heard at once when one of its episodes
/// plays. `None` is the normal speed.
pub(crate) async fn set_channel_speed(
    app: &AppHandle,
    channel_id: i64,
    speed: Option<f64>,
) -> AppResult<()> {
    let state = app.state::<AppState>();
    let pool = state.require_profile_pool().await?;
    let speed = speed
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.5, 2.0));
    if !store::set_channel_speed(&pool, channel_id, speed).await? {
        return Err(AppError::Other("podcast: channel_gone".into()));
    }
    if state
        .podcast_playback
        .current()
        .is_some_and(|entry| entry.channel_id == channel_id)
    {
        app.state::<Arc<AudioEngine>>()
            .send(AudioCmd::SetEpisodeSpeed(channel_speed(speed)))?;
    }
    emit_updated(app, Some(channel_id));
    Ok(())
}

/// What the probe is told, for a test of the mapping.
#[cfg(test)]
pub(crate) fn ext_hint_for_tests(content_type: Option<&str>) -> Option<&'static str> {
    stream::ext_hint(content_type)
}
