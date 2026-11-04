//! An episode of a bound server's channel, playing.
//!
//! The same session as a local episode ([`super::playback`]), so one detached queue
//! at a time and every surface that steps it holds without a copy of its rules. What
//! differs is where things are kept. Nothing here reads or writes a podcast row or a
//! `play_event`: the position is the server's, through its bookmark, and the server
//! keeps no played state, so an episode heard to its end loses its bookmark and opens
//! from the start, as the server's own web client has it.
//!
//! ## Requests come first
//!
//! A refusal changes nothing: what plays keeps playing. So [`play_episode`] reads the
//! channel before it takes the publish lock, and a step, which moved the cursor under
//! that lock, reads the episode after letting it go and rolls back if it lost its
//! place meanwhile, as the remote queue does. Holding the lock across a request would
//! park every other surface for the length of a network call.
//!
//! ## The ticket
//!
//! Not in the load, which is printed and kept for a rebuild, and which a ticket would
//! outlive: it is good for about an hour, which an episode easily is. The decoder asks
//! for one when it opens the stream, and again when a request answers that it does not
//! know the one it has ([`super::stream`]). Over the enclosure's reader, so it is in
//! no log line, redacted or not, and every request is asked about the switch, offline
//! mode and this server still being the one the profile is bound to.
//!
//! The original bytes, whatever the transcode preference says: a transcode cannot
//! seek, and resuming is a seek.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use url::Url;
use waveflow_core::podcast::address::parse_address;
use waveflow_core::podcast::FetchFailure;

use super::fetch::Fetcher;
use super::playback::{
    announce, display_title, notify_session_started, EpisodeEntry, EpisodeHome, EpisodeLoad,
    EpisodeSource, Step,
};
use super::refresh::PodcastError;
use super::store;
use super::stream::{EnclosureSource, Gate, Renew, StreamRefusal};
use crate::audio::engine::{AudioCmd, LoadIntent};
use crate::audio::AudioEngine;
use crate::error::{AppError, AppResult};
use crate::queue::{Direction, RepeatMode};
use crate::remote::client::RemoteClient;
use crate::remote::podcasts::{self, failure_word, ServerEpisode, ServerScope};
use crate::remote::stream::mint_ticket;
use crate::remote_playback::DetachedQueue;
use crate::state::AppState;

/// Emitted when a server's episode or channel has probably changed from here: a
/// position written at a pause, a seek or an end. Carries `channel_id`.
pub const PODCAST_SERVER_UPDATED: &str = "podcast:server-updated";

/// The word for an episode the server holds no file of.
const NOT_STORED: &str = "not_stored";

/// Where a server's episode is, for the session and the decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEpisodeRef {
    pub scope: ServerScope,
    pub episode_id: String,
    pub channel_id: String,
}

/// All of what writing one position may take: its turn, a token refresh, the request.
const POSITION_BUDGET: Duration = Duration::from_secs(8);

/// One write at a time, in the order they were asked for: a tick still on its way
/// must not land after the end that cleared the bookmark.
static POSITION_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The speed of a server's channel. The slider has no row of the channel to write to,
/// so this lasts as long as the app does: a voice keeps it through the queue, and it
/// never reaches `audio.playback_speed`.
static SPEEDS: LazyLock<Mutex<HashMap<(i64, String), f32>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn podcast_refusal(word: &str) -> AppError {
    AppError::Other(format!("podcast: {word}"))
}

/// Whether a request may leave for the server: the same two questions as for any
/// podcast, the switch first. The traffic goes to a host this person already chose, and
/// the rule stays one rule: off means no request.
pub(crate) async fn may_ask(pool: &sqlx::SqlitePool, offline: bool) -> Result<(), PodcastError> {
    if !store::is_enabled(pool).await? {
        return Err(PodcastError::Disabled);
    }
    if offline {
        return Err(PodcastError::Offline);
    }
    Ok(())
}

/// A client for the active profile's server, once the switch and offline mode have
/// had their say. Nothing is contacted before both have, a token refresh included.
pub(crate) async fn ready_client(state: &AppState) -> AppResult<Option<(RemoteClient<'_>, i64)>> {
    let profile_id = {
        let (pool, profile_id) = state.require_profile_snapshot().await?;
        may_ask(&pool, crate::offline::is_offline()).await?;
        profile_id
        // The lease ends here: holding it across a request would stall a switch.
    };
    Ok(RemoteClient::try_build_for(state, profile_id)
        .await?
        .map(|client| (client, profile_id)))
}

/// A client still speaking for `scope`, or `None`: another profile, another server,
/// another account, signed out.
async fn client_in_scope<'a>(state: &'a AppState, scope: &ServerScope) -> Option<RemoteClient<'a>> {
    RemoteClient::try_build_for(state, scope.profile_id)
        .await
        .ok()
        .flatten()
        .filter(|client| scope.holds_for(client))
}

fn entry_of(episode: &ServerEpisode, scope: &ServerScope) -> EpisodeEntry {
    EpisodeEntry {
        home: EpisodeHome::Server(Box::new(ServerEpisodeRef {
            scope: scope.clone(),
            episode_id: episode.id.clone(),
            channel_id: episode.channel_id.clone(),
        })),
        profile_id: scope.profile_id,
        title: display_title(&episode.title, &episode.channel_title),
        channel: episode.channel_title.clone(),
        duration_ms: episode.duration_ms,
    }
}

/// The queue a channel's episodes form, newest first as the server lists them: what
/// is stored, since nothing else plays. A word when `current` is not in it, or not
/// stored.
pub(crate) fn queue_of(
    episodes: &[ServerEpisode],
    current: &str,
    scope: &ServerScope,
) -> Result<(Vec<EpisodeEntry>, usize), &'static str> {
    let wanted = episodes
        .iter()
        .find(|episode| episode.id == current)
        .ok_or("episode_gone")?;
    if !wanted.is_stored() {
        return Err(NOT_STORED);
    }
    let stored: Vec<&ServerEpisode> = episodes.iter().filter(|e| e.is_stored()).collect();
    let index = stored
        .iter()
        .position(|episode| episode.id == current)
        .unwrap_or(0);
    Ok((
        stored
            .into_iter()
            .map(|episode| entry_of(episode, scope))
            .collect(),
        index,
    ))
}

/// The word for a read of one episode that failed: the server knows nothing of it,
/// or something else went wrong.
fn read_refusal(failure: &crate::remote::client::RemoteFailure) -> AppError {
    match failure.status {
        Some(404) => PodcastError::EpisodeGone.into(),
        _ => podcast_refusal(&failure_word(failure)),
    }
}

/// The cover, in the cache of this profile's server covers. Asks the server the first
/// time and not after. A cover nobody could fetch is a player with no picture.
async fn artwork_path(state: &AppState, hash: Option<&str>) -> Option<String> {
    crate::remote::artwork::cached_path(state, hash?)
        .await
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn load_of(
    episode: &ServerEpisode,
    scope: &ServerScope,
    artwork_path: Option<String>,
) -> EpisodeLoad {
    let duration_ms = episode.duration_ms.unwrap_or(0).max(0) as u64;
    let channel_id = episode.channel_id.clone();
    let speed = SPEEDS
        .lock()
        .ok()
        .and_then(|speeds| speeds.get(&(scope.profile_id, channel_id)).copied())
        .unwrap_or(1.0);
    EpisodeLoad {
        home: EpisodeHome::Server(Box::new(ServerEpisodeRef {
            scope: scope.clone(),
            episode_id: episode.id.clone(),
            channel_id: episode.channel_id.clone(),
        })),
        profile_id: scope.profile_id,
        track_id: crate::commands::player::next_radio_track_id(),
        start_ms: crate::queue::resume_position(
            episode.position_ms.unwrap_or(0).max(0) as u64,
            duration_ms,
        ),
        duration_ms,
        title: display_title(&episode.title, &episode.channel_title),
        channel: episode.channel_title.clone(),
        artwork_path,
        speed,
        source: EpisodeSource::Server {
            declared_type: episode.content_type.clone(),
        },
    }
}

/// Starts an episode, with its channel's stored episodes as the queue. A refusal
/// changes nothing: `not_stored` for one the server holds no file of.
pub(crate) async fn play_episode(
    app: &AppHandle,
    channel_id: &str,
    episode_id: &str,
) -> AppResult<()> {
    let engine = app.state::<Arc<AudioEngine>>();
    // Claimed before anything is read (#622): a track picked while this prepares is
    // the newer choice.
    let intent = engine.next_load_intent();
    let state = app.state::<AppState>();
    let (client, profile_id) = ready_client(&state)
        .await?
        .ok_or_else(|| podcast_refusal("server_signed_out"))?;
    let scope =
        ServerScope::of(&client, profile_id).ok_or_else(|| podcast_refusal("server_signed_out"))?;
    let channel = podcasts::channel(&client, channel_id)
        .await
        .map_err(|err| {
            tracing::debug!(status = ?err.status, "podcast: a server channel could not be read");
            read_refusal(&err)
        })?;
    let episodes = channel.episodes.unwrap_or_default();
    let (entries, index) = queue_of(&episodes, episode_id, &scope).map_err(podcast_refusal)?;
    let episode = episodes
        .iter()
        .find(|episode| episode.id == episode_id)
        .ok_or(PodcastError::EpisodeGone)?;
    let artwork = artwork_path(&state, episode.artwork_hash.as_deref()).await;
    let load = load_of(episode, &scope, artwork);

    // Everything from here on is local.
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        tracing::debug!("server episode superseded before it started; not installing it");
        return Ok(());
    }
    // One detached queue at a time.
    state.remote_playback.clear();
    state.external_playback.clear();
    let revision = state.podcast_playback.set(DetachedQueue { entries, index });
    notify_session_started();
    announce(app, &load);
    let played = engine.send(AudioCmd::LoadEpisodeAndPlay {
        intent,
        episode: Box::new(load),
    });
    if played.is_err() {
        let _ = engine.send(AudioCmd::Stop);
        state.podcast_playback.clear_if(revision);
    } else if engine.load_intent_superseded(intent) {
        state.podcast_playback.clear_if(revision);
    }
    played
}

/// Reads the episode under the cursor again and plays it from where the server says.
/// For a step, a jump and Play from a stopped player. `Ok` without a load when a newer
/// pick overtook it during the read.
async fn play_current(app: &AppHandle, intent: LoadIntent) -> AppResult<()> {
    let state = app.state::<AppState>();
    let Some(entry) = state.podcast_playback.current() else {
        return Ok(());
    };
    let EpisodeHome::Server(place) = &entry.home else {
        return Ok(());
    };
    {
        let pool = state
            .require_profile_pool_for(Some(place.scope.profile_id))
            .await
            .map_err(|_| PodcastError::ProfileChanged)?;
        may_ask(&pool, crate::offline::is_offline()).await?;
    }
    let client = client_in_scope(&state, &place.scope)
        .await
        .ok_or(PodcastError::ProfileChanged)?;
    let episode = podcasts::episode(&client, &place.episode_id)
        .await
        .map_err(|err| read_refusal(&err))?;
    if !episode.is_stored() {
        return Err(podcast_refusal(NOT_STORED));
    }
    let artwork = artwork_path(&state, episode.artwork_hash.as_deref()).await;
    let engine = app.state::<Arc<AudioEngine>>();
    if engine.load_intent_superseded(intent) {
        return Ok(());
    }
    let load = load_of(&episode, &place.scope, artwork);
    announce(app, &load);
    engine.send(AudioCmd::LoadEpisodeAndPlay {
        intent,
        episode: Box::new(load),
    })
}

/// Steps the cursor and plays what it lands on. The server keeps no played state, so
/// an episode that ended leads to the next stored one, heard or not.
pub(crate) async fn advance(app: &AppHandle, step: Step, intent: LoadIntent) -> AppResult<bool> {
    let state = app.state::<AppState>();
    let repeat = match state.require_profile_pool().await {
        Ok(pool) => crate::queue::read_repeat_mode(&pool).await,
        Err(_) => RepeatMode::Off,
    };
    let engine = app.state::<Arc<AudioEngine>>();
    {
        // A step overtaken by a newer pick moves nothing (#632).
        let _publish = engine.lock_publish().await;
        if !engine.claim_dispatch(intent) {
            return Ok(false);
        }
        let direction = match step {
            Step::Asked(direction) => direction,
            Step::Ended => Direction::Next,
        };
        if state.podcast_playback.step(direction, repeat).is_none() {
            let _ = engine.send(AudioCmd::Stop);
            return Ok(false);
        }
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
    let state = app.state::<AppState>();
    {
        let _publish = engine.lock_publish().await;
        if !engine.claim_dispatch(intent) {
            return Ok(());
        }
        if state.podcast_playback.seek_to(index).is_none() {
            return Ok(());
        }
    }
    play_current(app, intent).await.inspect_err(|_| {
        let _ = engine.send(AudioCmd::Stop);
        state.podcast_playback.clear();
    })
}

/// Play from a stopped player while a server's episode is the session: from where the
/// server says it was left.
pub(crate) async fn resume(app: &AppHandle, intent: LoadIntent) -> AppResult<()> {
    let engine = app.state::<Arc<AudioEngine>>();
    {
        let _publish = engine.lock_publish().await;
        if !engine.claim_dispatch(intent) {
            return Ok(());
        }
    }
    play_current(app, intent).await
}

/// What a position comes to on the server: a bookmark, or none once it counts as
/// heard. Pure, so the rule is tested without one.
pub(crate) fn position_write(position_ms: u64, duration_ms: u64, finished: bool) -> Option<u64> {
    if finished || super::playback::counts_as_played(position_ms, duration_ms) {
        None
    } else {
        Some(position_ms)
    }
}

/// Writes where the episode stands to the account that was listening, or to nobody.
/// With the switch off or offline mode on nothing is sent, so a position not yet
/// written is lost: there is no local row to hold it for later, and it is never kept
/// as one. Failures are the same, and are only logged by their status.
pub(crate) async fn record_position(
    app: &AppHandle,
    place: &ServerEpisodeRef,
    position_ms: u64,
    duration_ms: u64,
    finished: bool,
    announce_it: bool,
) {
    let state = app.state::<AppState>();
    {
        let Ok(pool) = state
            .require_profile_pool_for(Some(place.scope.profile_id))
            .await
        else {
            tracing::debug!("podcast: server position dropped, its profile is gone");
            return;
        };
        if may_ask(&pool, crate::offline::is_offline()).await.is_err() {
            return;
        }
    }
    let write = async {
        let _order = POSITION_WRITES.lock().await;
        let Some(client) = client_in_scope(&state, &place.scope).await else {
            tracing::debug!("podcast: server position dropped, its server is no longer bound");
            return;
        };
        let written = match position_write(position_ms, duration_ms, finished) {
            Some(position) => podcasts::save_position(&client, &place.episode_id, position).await,
            None => podcasts::clear_position(&client, &place.episode_id).await,
        };
        if let Err(err) = written {
            tracing::debug!(status = ?err.status, "podcast: server position not written");
        }
    };
    // This runs on the task that also steps to the next episode, and a token refresh
    // on a server that hangs would otherwise take the whole of its own deadline.
    if tokio::time::timeout(POSITION_BUDGET, write).await.is_err() {
        tracing::debug!("podcast: server position not written in time");
    }
    if announce_it {
        let _ = app.emit(
            PODCAST_SERVER_UPDATED,
            serde_json::json!({ "channel_id": place.channel_id }),
        );
    }
}

/// A speed moved during a server's episode: heard at once, and kept for the channel
/// while the app runs.
pub(crate) fn set_channel_speed(
    app: &AppHandle,
    place: &ServerEpisodeRef,
    speed: f32,
) -> AppResult<()> {
    if let Ok(mut speeds) = SPEEDS.lock() {
        speeds.insert((place.scope.profile_id, place.channel_id.clone()), speed);
    }
    app.state::<Arc<AudioEngine>>()
        .send(AudioCmd::SetEpisodeSpeed(speed))
}

/// Asked before every request of a stream: the switch, offline mode, and whether this
/// is still the server the profile is bound to.
async fn may_stream(app: &AppHandle, scope: &ServerScope) -> Result<(), StreamRefusal> {
    if crate::offline::is_offline() {
        return Err(StreamRefusal::Offline);
    }
    let state = app.state::<AppState>();
    let pool = state
        .require_profile_pool_for(Some(scope.profile_id))
        .await
        .map_err(|_| StreamRefusal::Released)?;
    if !store::is_enabled(&pool).await.unwrap_or(false) {
        return Err(StreamRefusal::Disabled);
    }
    let mut conn = pool.acquire().await.map_err(|_| StreamRefusal::Released)?;
    match scope.is_current(&mut conn).await {
        Ok(true) => Ok(()),
        // Nothing here is for whoever is bound now, so nothing to say to them.
        Ok(false) | Err(_) => Err(StreamRefusal::Released),
    }
}

/// A ticket for the episode, as an address.
async fn mint(
    app: &AppHandle,
    scope: &ServerScope,
    episode_id: &str,
) -> Result<Url, StreamRefusal> {
    let state = app.state::<AppState>();
    let client = client_in_scope(&state, scope)
        .await
        .ok_or(StreamRefusal::Released)?;
    let address = mint_ticket(&client, episode_id)
        .await
        .map_err(|err| match err.status {
            // Stored a moment ago and no longer, or the server's own ticket rules.
            Some(404) => StreamRefusal::NotStored,
            _ => StreamRefusal::Failed(FetchFailure::Unreached),
        })?;
    parse_address(&address).ok_or(StreamRefusal::Failed(FetchFailure::AddressRefused))
}

/// Opens a server's episode for the decoder. Blocking: for its thread alone.
pub(crate) fn open_stream(
    app: &AppHandle,
    place: &ServerEpisodeRef,
) -> Result<EnclosureSource, StreamRefusal> {
    let fetcher = Fetcher::new().map_err(StreamRefusal::from)?;
    let gate: Gate = {
        let (app, scope) = (app.clone(), place.scope.clone());
        Arc::new(move || {
            let (app, scope) = (app.clone(), scope.clone());
            Box::pin(async move { may_stream(&app, &scope).await })
        })
    };
    let renew: Renew = {
        let (app, scope, episode_id) = (app.clone(), place.scope.clone(), place.episode_id.clone());
        Arc::new(move || {
            let (app, scope, episode_id) = (app.clone(), scope.clone(), episode_id.clone());
            Box::pin(async move { mint(&app, &scope, &episode_id).await })
        })
    };
    EnclosureSource::open_renewing(
        tauri::async_runtime::handle().inner().clone(),
        fetcher,
        gate,
        renew,
        place.scope.origin(),
    )
}
